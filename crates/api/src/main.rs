//! HTTP API: runs backtests and serves the WASM frontend.
//!   GET /api/backtest?ticker=AAPL&strategy=sma_crossover&fast=20&slow=50&years=10

use std::sync::{Arc, Mutex};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use bagholder_core::{
    econ_cycle_alloc, inverse_vol_alloc, local_minima, momentum_alloc,
    pairs_alloc, pe_history, pe_series, run_event_backtest, run_signals_backtest, squeeze_signals,
    run_multi_asset_backtest,
    run_portfolio_backtest, Bar, BacktestResult, BandConfig, Candidate, CongressTrade,
    run_portfolio_backtest_taxed, is_fund_type,
    CorporateAction, FillCosts, FundTax, Fundamental, PeHistory, RebalanceConfig, Strategy, TaxConfig, TaxSystem, SECTOR_ETFS,
};
use std::collections::HashMap;
use bagholder_data::Store;
use serde::Deserialize;
use tower_http::{compression::CompressionLayer, cors::CorsLayer, services::ServeDir};

// ponytail: one global lock around the DuckDB connection — fine for single-user
// dev. Swap for a connection pool if concurrent throughput ever matters.
type Db = Arc<Mutex<Store>>;

#[derive(Deserialize)]
struct BacktestQuery {
    ticker: String,
    #[serde(default = "default_strategy")]
    strategy: String,
    fast: Option<usize>,
    slow: Option<usize>,
    /// Trim to the last N years before running; omitted or 0 = full history.
    years: Option<u32>,
    /// "pe_min" enters at a local-minimum P/E (overrides `years`).
    entry: Option<String>,
    /// Trough window for `entry=pe_min`, in trading days (default ~a quarter).
    pe_window: Option<usize>,
    /// For strategy=congress_copy_trade: which year's disclosures to use (default: 2023).
    year: Option<u32>,
    /// For strategy=congress_copy_trade: true = use filing date (realistic), false = transaction date (naive).
    use_filing_date: Option<bool>,
    /// Which trough to enter, counting back from most recent (0 = latest).
    /// Clamped to the available range.
    pe_index: Option<usize>,
    // BuyTheDip params (strategy=buy_the_dip)
    rsi_period: Option<usize>,
    rsi_threshold: Option<f64>,
    bb_period: Option<usize>,
    bb_std: Option<f64>,
    /// Initial investment in dollars; defaults to $10 000.
    initial_amount: Option<f64>,
    /// Benchmark ticker to run a comparison backtest against (default: no benchmark).
    benchmark_ticker: Option<String>,
    /// Benchmark strategy string (default: "buy_and_hold").
    benchmark_strategy: Option<String>,
    /// Tax regime: "us" | "de" | "none" (default).
    tax: Option<String>,
    /// US: annual taxable income (drives the LT bracket + NIIT cliff).
    tax_income: Option<f64>,
    /// DE: church-tax flag (26.375% → ~27.82%).
    tax_church: Option<bool>,
    /// DE: annual tax-free allowance (Sparerpauschbetrag).
    tax_allowance: Option<f64>,
    /// DE: "treat all ETFs as equity funds" estimate toggle (#61 estimate mode).
    tax_estimate: Option<bool>,
    /// DE: Teilfreistellung percent applied to funds in estimate mode (default 30).
    tax_teilfrei: Option<f64>,
    /// DE: accrue the Vorabpauschale (default on for DE).
    tax_vorab: Option<bool>,
    /// Realize + tax all open positions on the final bar (default on).
    tax_sellall: Option<bool>,
}

/// Map the `tax=` query value to a `TaxSystem`; anything unrecognized = None.
fn tax_system(tax: Option<&str>) -> TaxSystem {
    match tax {
        Some("us") => TaxSystem::UsFederal,
        Some("de") => TaxSystem::Germany,
        _ => TaxSystem::None,
    }
}

/// When a tax system is active, run a pre-tax baseline on the same bars and
/// attach it so the UI can pair after-tax against pre-tax. No-op for `None`.
fn attach_pretax(
    r: &mut BacktestResult, system: TaxSystem, ticker: &str, bars: &[Bar],
    strategy: &Strategy, amount: f64, actions: &[CorporateAction],
) {
    if system == TaxSystem::None {
        return;
    }
    let pretax = run_portfolio_backtest(ticker, bars, strategy, amount, &FillCosts::ZERO, 0.0, actions)
        .with_amount(amount);
    r.pretax = Some(Box::new(pretax));
}

fn default_strategy() -> String {
    "buy_and_hold".into()
}

/// Pull quarterly EPS as `(known_from_date, value)`.
/// Uses the SEC filing date when available (point-in-time); falls back to
/// period end for facts from older filings not in the submissions cache.
fn quarterly_eps(funds: &[Fundamental]) -> Vec<(chrono::NaiveDate, f64)> {
    funds
        .iter()
        .filter(|f| f.metric == "eps_basic" && f.period_type == "Q")
        .map(|f| (f.filed_date.unwrap_or(f.period), f.value))
        .collect()
}

/// Keep only the last `years` of bars (relative to the most recent bar, so it
/// works the same whether or not the cache is current).
fn trim_years(mut bars: Vec<Bar>, years: Option<u32>) -> Vec<Bar> {
    if let (Some(y), Some(last)) = (years.filter(|y| *y > 0), bars.last()) {
        let cutoff = last.date - chrono::Duration::days(365 * y as i64);
        bars.retain(|b| b.date >= cutoff);
    }
    bars
}

/// Convert congress trade records to `(execution_date, weight)` signal events.
/// `weight` = 1.0 for purchases, 0.0 for sales. Caller chooses the date field.
fn congress_disclosures(
    trades: &[CongressTrade],
    ticker: &str,
    use_filing_date: bool,
) -> Vec<(chrono::NaiveDate, f64)> {
    let mut events: Vec<(chrono::NaiveDate, f64)> = trades
        .iter()
        .filter(|t| t.ticker.eq_ignore_ascii_case(ticker))
        .map(|t| {
            let date = if use_filing_date { t.filing_date } else { t.transaction_date };
            let weight = if t.trade_type.contains("sale") { 0.0 } else { 1.0 };
            (date, weight)
        })
        .collect();
    events.sort_by_key(|(d, _)| *d);
    events
}

async fn backtest(
    State(db): State<Db>,
    Query(q): Query<BacktestQuery>,
) -> Result<Json<BacktestResult>, (StatusCode, String)> {
    // Inverse Cramer: separate path — fades Cramer calls (buy→short, sell→long).
    if q.strategy == "cramer_inverse" {
        let ticker = q.ticker.clone();
        let (bars, calls) = tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            let bars = db.ohlcv(&ticker)?;
            let calls = db.cramer_calls(&ticker)?;
            Ok::<_, anyhow::Error>((bars, calls))
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;

        let mut events: Vec<(chrono::NaiveDate, f64)> = calls
            .iter()
            .map(|c| (c.date, if c.call == "buy" { -1.0 } else { 1.0 }))
            .collect();
        events.sort_by_key(|(d, _)| *d);
        let bars = trim_years(bars, q.years);
        let amount = q.initial_amount.unwrap_or(10_000.0);
        return Ok(Json(run_event_backtest(&q.ticker, &bars, &events).with_amount(amount)));
    }

    // Short squeeze: high days-to-cover + upward momentum entry.
    // ponytail: dtc_min=5 and window=20 hardcoded; add query params if users want knobs.
    if q.strategy == "short_squeeze" {
        let ticker = q.ticker.clone();
        let (bars, si) = tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            let bars = db.ohlcv(&ticker)?;
            let si = db.short_interest(&ticker)?;
            Ok::<_, anyhow::Error>((bars, si))
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;

        let si_events: Vec<(chrono::NaiveDate, f64)> = si
            .iter()
            .map(|r| (r.settlement_date, r.days_to_cover))
            .collect();
        let bars = trim_years(bars, q.years);
        let sigs = squeeze_signals(&bars, &si_events, 5.0, 20);
        let amount = q.initial_amount.unwrap_or(10_000.0);
        return Ok(Json(run_signals_backtest(&q.ticker, &bars, &sigs).with_amount(amount)));
    }

    // Congress copy-trade: separate path — uses external disclosure signals.
    if q.strategy == "congress_copy_trade" {
        let ticker = q.ticker.clone();
        let year = q.year.unwrap_or(2023);
        let use_filing = q.use_filing_date.unwrap_or(false);
        let (bars, trades) = tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            let bars = db.ohlcv(&ticker)?;
            let trades = db.congress_trades(year)?;
            Ok::<_, anyhow::Error>((bars, trades))
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;

        let disclosures = congress_disclosures(&trades, &q.ticker, use_filing);
        let bars = trim_years(bars, q.years);
        let amount = q.initial_amount.unwrap_or(10_000.0);
        return Ok(Json(run_event_backtest(&q.ticker, &bars, &disclosures).with_amount(amount)));
    }

    let strategy = match q.strategy.as_str() {
        "sma_crossover" => Strategy::SmaCrossover {
            fast: q.fast.unwrap_or(20),
            slow: q.slow.unwrap_or(50),
        },
        "buy_the_dip" => Strategy::BuyTheDip {
            rsi_period: q.rsi_period.unwrap_or(14),
            rsi_threshold: q.rsi_threshold.unwrap_or(20.0),
            bb_period: q.bb_period.unwrap_or(20),
            bb_std: q.bb_std.unwrap_or(2.0),
        },
        "regime_mean_reversion" => Strategy::RegimeMeanReversion {
            rsi_period: q.rsi_period.unwrap_or(14),
            rsi_entry: q.rsi_threshold.unwrap_or(30.0),
            rsi_exit: 70.0,
            adx_period: 14,
            adx_threshold: 25.0,
        },
        _ => Strategy::BuyAndHold,
    };

    let ticker = q.ticker.clone();
    let pe_min = q.entry.as_deref() == Some("pe_min");
    let system = tax_system(q.tax.as_deref());
    let db_main = db.clone();
    // Blocking DB + network I/O must not run on the async runtime's workers.
    let (bars, eps, actions, instrument_type) = tokio::task::spawn_blocking(move || {
        let db = db_main.lock().unwrap();
        let bars = db.ohlcv(&ticker)?;
        // Only the fundamentals fetch is conditional — skip it for plain runs.
        let eps = if pe_min {
            quarterly_eps(&db.fundamentals(&ticker)?)
        } else {
            Vec::new()
        };
        let actions: Vec<CorporateAction> = db.corporate_actions(&ticker)?;
        // German fund taxation needs the instrument type (rides the cached ohlcv).
        let instrument_type = if system == TaxSystem::Germany {
            db.instrument_type(&ticker).ok().flatten()
        } else {
            None
        };
        Ok::<_, anyhow::Error>((bars, eps, actions, instrument_type))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;

    let amount = q.initial_amount.unwrap_or(10_000.0);
    let bench_ticker  = q.benchmark_ticker.clone();
    let bench_strat   = q.benchmark_strategy.clone();
    let years         = q.years;
    // Tax regime: preset for the system, then the user's knobs from the query.
    // Realized tax is applied to the main run; the benchmark stays pre-tax.
    let mut tax_cfg = TaxConfig::preset(system);
    if let Some(v) = q.tax_income { tax_cfg.taxable_income = v; }
    if let Some(v) = q.tax_church { tax_cfg.church_tax = v; }
    if let Some(v) = q.tax_allowance { tax_cfg.annual_allowance = v; }
    if let Some(v) = q.tax_estimate { tax_cfg.estimate_all_etfs_equity = v; }
    if let Some(v) = q.tax_vorab { tax_cfg.vorabpauschale = v; }
    if let Some(v) = q.tax_sellall { tax_cfg.sell_all = v; }
    let tfs_frac = q.tax_teilfrei.map(|p| p / 100.0).unwrap_or(0.30);
    // Flag the ticker as a German fund (→ Vorabpauschale; Teilfreistellung only
    // when the estimate toggle is on). A direct stock stays `None`.
    let fund = if system == TaxSystem::Germany
        && instrument_type.as_deref().map(is_fund_type).unwrap_or(false)
    {
        let tfs = if tax_cfg.estimate_all_etfs_equity { tfs_frac } else { 0.0 };
        Some(FundTax { teilfreistellung: tfs, distributing: false })
    } else {
        None
    };
    let mut result = if pe_min {
        let series = pe_series(&bars, &eps);
        let window = q.pe_window.unwrap_or(63);
        let minima = local_minima(&series, window);
        if minima.is_empty() {
            return Err((StatusCode::UNPROCESSABLE_ENTITY,
                "no P/E history to find a minimum (missing EPS?)".into()));
        }
        let count = minima.len();
        let k = q.pe_index.unwrap_or(0).min(count - 1);
        let (entry_date, entry_pe) = series[minima[count - 1 - k]];
        let trimmed: Vec<Bar> = bars.into_iter().filter(|b| b.date >= entry_date).collect();
        let mut r = run_portfolio_backtest_taxed(&q.ticker, &trimmed, &strategy, amount, &FillCosts::ZERO, 0.0, &actions, &tax_cfg, fund.as_ref())
            .with_amount(amount);
        r.entry_date = Some(entry_date);
        r.entry_pe = Some(entry_pe);
        r.entry_index = Some(k);
        r.entry_count = Some(count);
        attach_pretax(&mut r, system, &q.ticker, &trimmed, &strategy, amount, &actions);
        r
    } else {
        let run_bars = trim_years(bars, q.years);
        let mut r = run_portfolio_backtest_taxed(
            &q.ticker, &run_bars, &strategy, amount, &FillCosts::ZERO, 0.0, &actions, &tax_cfg, fund.as_ref(),
        ).with_amount(amount);
        attach_pretax(&mut r, system, &q.ticker, &run_bars, &strategy, amount, &actions);
        r
    };

    // Optional benchmark run — a second buy-and-hold (or configured strategy) on a separate ticker.
    if let Some(bt) = bench_ticker {
        let db2 = db.clone();
        let bt2 = bt.clone();
        let bench_result = tokio::task::spawn_blocking(move || {
            let db = db2.lock().unwrap();
            let bars = db.ohlcv(&bt2)?;
            let actions: Vec<CorporateAction> = db.corporate_actions(&bt2)?;
            Ok::<_, anyhow::Error>((bars, actions))
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        let (bench_bars, bench_actions) = bench_result;
        let bench_strategy = match bench_strat.as_deref().unwrap_or("buy_and_hold") {
            "sma_crossover" => Strategy::SmaCrossover { fast: 20, slow: 50 },
            _ => Strategy::BuyAndHold,
        };
        let b = run_portfolio_backtest(
            &bt,
            &trim_years(bench_bars, years),
            &bench_strategy,
            amount,
            &FillCosts::ZERO,
            0.0,
            &bench_actions,
        ).with_amount(amount);
        result.benchmark = Some(Box::new(b));
    }

    Ok(Json(result))
}

#[derive(Deserialize)]
struct TickerQuery {
    ticker: String,
}

async fn fundamentals(
    State(db): State<Db>,
    Query(q): Query<TickerQuery>,
) -> Result<Json<Vec<Fundamental>>, (StatusCode, String)> {
    let ticker = q.ticker.clone();
    let funds = tokio::task::spawn_blocking(move || db.lock().unwrap().fundamentals(&ticker))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    Ok(Json(funds))
}

#[derive(Deserialize)]
struct PeHistoryQuery {
    ticker: String,
    pe_window: Option<usize>,
}

async fn pe_history_handler(
    State(db): State<Db>,
    Query(q): Query<PeHistoryQuery>,
) -> Result<Json<PeHistory>, (StatusCode, String)> {
    let ticker = q.ticker.clone();
    let window = q.pe_window.unwrap_or(63);
    let (bars, eps) = tokio::task::spawn_blocking(move || {
        let db = db.lock().unwrap();
        let bars = db.ohlcv(&ticker)?;
        let eps = quarterly_eps(&db.fundamentals(&ticker)?);
        Ok::<_, anyhow::Error>((bars, eps))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Json(pe_history(&bars, &eps, window)))
}

#[derive(Deserialize)]
struct ScreenQuery {
    // Only "low_pe" exists today; kept so the URL is self-describing and future
    // screens slot in without a breaking change.
    #[serde(default)]
    kind: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

async fn screen(
    State(db): State<Db>,
    Query(q): Query<ScreenQuery>,
) -> Result<Json<Vec<Candidate>>, (StatusCode, String)> {
    if !q.kind.is_empty() && q.kind != "low_pe" {
        return Err((StatusCode::BAD_REQUEST, format!("unknown screen: {}", q.kind)));
    }
    let candidates = tokio::task::spawn_blocking(move || {
        bagholder_data::low_pe(&db.lock().unwrap(), bagholder_data::DEFAULT_UNIVERSE, q.limit)
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Json(candidates))
}

/// Returns all US ticker symbols from SEC's directory as a JSON array of strings.
async fn universe(State(db): State<Db>) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let tickers = tokio::task::spawn_blocking(move || db.lock().unwrap().all_tickers())
        .await
        .map_err(internal)?
        .map_err(internal)?;
    Ok(Json(tickers))
}

/// Multi-asset preset backtests: `GET /api/preset?kind=risk_parity&tickers=SPY,QQQ,GLD`
///
/// Currently supported: `kind=risk_parity` (inverse-volatility weights, monthly rebalance).
/// Returns a single aggregate `BacktestResult` (one equity curve for the whole portfolio).
#[derive(Deserialize)]
struct PresetQuery {
    kind: String,
    /// Comma-separated Yahoo tickers. Defaults to SPY,QQQ,GLD,TLT,IEF if omitted.
    tickers: Option<String>,
    /// Trailing window for vol estimate in trading days (default 20, risk_parity only).
    vol_window: Option<usize>,
    /// Momentum lookback in trading days (default 126 ≈ 6 months, sector_rotation only).
    lookback: Option<usize>,
    /// Top-N sectors to hold (default 3, sector_rotation only).
    top_n: Option<usize>,
    /// Pairs: first ticker of the pair (pairs only).
    ticker_a: Option<String>,
    /// Pairs: second ticker of the pair (pairs only).
    ticker_b: Option<String>,
    /// Pairs: z-score entry threshold (default 2.0, pairs only).
    entry_z: Option<f64>,
    /// Calendar rebalance interval in days (default 30).
    rebalance_days: Option<u32>,
}

async fn preset_backtest(
    State(db): State<Db>,
    Query(q): Query<PresetQuery>,
) -> Result<Json<BacktestResult>, (StatusCode, String)> {
    // "pairs" has its own flow — handle early.
    if q.kind == "pairs" {
        let ta = q.ticker_a.clone().unwrap_or_else(|| "KO".to_string()).to_uppercase();
        let tb = q.ticker_b.clone().unwrap_or_else(|| "PEP".to_string()).to_uppercase();
        let entry_z = q.entry_z.unwrap_or(2.0);
        let win = q.lookback.unwrap_or(60);
        let ta2 = ta.clone(); let tb2 = tb.clone();
        let bars_by_ticker: HashMap<String, Vec<Bar>> = tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            Ok::<_, anyhow::Error>(HashMap::from([(ta.clone(), db.ohlcv(&ta)?), (tb.clone(), db.ohlcv(&tb)?)]))
        }).await.map_err(internal)?.map_err(internal)?;
        let cfg = RebalanceConfig {
            calendar_days: Some(q.rebalance_days.unwrap_or(5)),
            bands: Some(BandConfig { absolute: 0.05, relative: 0.25 }),
            full: true,
        };
        let result = run_multi_asset_backtest(
            &bars_by_ticker,
            move |history, _| pairs_alloc(history, &ta2, &tb2, win, entry_z),
            &cfg, 10_000.0, &FillCosts::ZERO, 0.0,
        );
        return Ok(Json(result));
    }

    // Economic-cycle rotation needs FRED data — handle early.
    if q.kind == "econ_cycle" {
        let ticker_list: Vec<String> = SECTOR_ETFS.iter().map(|s| s.to_string()).collect();
        let rebalance_config = RebalanceConfig {
            calendar_days: Some(q.rebalance_days.unwrap_or(30)),
            bands: Some(BandConfig { absolute: 0.05, relative: 0.25 }),
            full: true,
        };
        let (bars_by_ticker, t10y2y) = tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            let bars = ticker_list.iter()
                .map(|t| Ok::<_, anyhow::Error>((t.clone(), db.ohlcv(t)?)))
                .collect::<Result<HashMap<_, _>, _>>()?;
            let macro_data = db.macro_series("T10Y2Y")?;
            Ok::<_, anyhow::Error>((bars, macro_data))
        }).await.map_err(internal)?.map_err(internal)?;

        let result = run_multi_asset_backtest(
            &bars_by_ticker,
            move |history, _| {
                let current_date = history.values()
                    .filter_map(|bars| bars.last().map(|b| b.date))
                    .max();
                let spread = current_date.and_then(|d| {
                    t10y2y.iter().filter(|(td, _)| *td <= d).last().map(|(_, v)| *v)
                });
                econ_cycle_alloc(history, spread)
            },
            &rebalance_config,
            10_000.0,
            &FillCosts::ZERO,
            0.0,
        );
        return Ok(Json(result));
    }

    let (ticker_list, alloc_kind) = match q.kind.as_str() {
        "risk_parity" => (
            q.tickers.as_deref().unwrap_or("SPY,QQQ,GLD,TLT,IEF")
                .split(',').map(|s| s.trim().to_uppercase()).collect::<Vec<_>>(),
            "risk_parity",
        ),
        "sector_rotation" => (
            SECTOR_ETFS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "sector_rotation",
        ),
        other => return Err((StatusCode::BAD_REQUEST, format!("unknown preset: {other}"))),
    };

    let vol_window = q.vol_window.unwrap_or(20);
    let lookback = q.lookback.unwrap_or(126);
    let top_n = q.top_n.unwrap_or(3);
    let rebalance_config = RebalanceConfig {
        calendar_days: Some(q.rebalance_days.unwrap_or(30)),
        bands: Some(BandConfig { absolute: 0.05, relative: 0.25 }),
        full: true,
    };

    let bars_by_ticker: HashMap<String, Vec<Bar>> = tokio::task::spawn_blocking(move || {
        let db = db.lock().unwrap();
        ticker_list.iter()
            .map(|t| Ok::<_, anyhow::Error>((t.clone(), db.ohlcv(t)?)))
            .collect::<Result<HashMap<_, _>, _>>()
    }).await.map_err(internal)?.map_err(internal)?;

    let result = run_multi_asset_backtest(
        &bars_by_ticker,
        move |history, _i| match alloc_kind {
            "sector_rotation" => momentum_alloc(history, lookback, top_n),
            _ => inverse_vol_alloc(history, vol_window),
        },
        &rebalance_config,
        10_000.0,
        &FillCosts::ZERO,
        0.0,
    );
    Ok(Json(result))
}

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[tokio::main]
async fn main() {
    let db: Db = Arc::new(Mutex::new(
        Store::open(&format!(
            "{}/bagholder.duckdb",
            std::env::var("DATA_DIR").unwrap_or_else(|_| ".".to_string())
        )).expect("opening data store"),
    ));

    let app = Router::new()
        .route("/api/backtest", get(backtest))
        .route("/api/preset", get(preset_backtest))
        .route("/api/fundamentals", get(fundamentals))
        .route("/api/pe_history", get(pe_history_handler))
        .route("/api/screen", get(screen))
        .route("/api/universe", get(universe))
        // Serve the trunk-built frontend. Run `trunk build` in crates/web first.
        .fallback_service(ServeDir::new("crates/web/dist"))
        // gzip/brotli-encode responses — the ~629 KB wasm ships ~205 KB gzipped.
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .with_state(db);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(addr.as_str()).await.unwrap();
    println!("listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}
