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
    local_minima, pe_history, pe_series, run_portfolio_backtest, Bar, BacktestResult, Candidate,
    FillCosts, Fundamental, PeHistory, Strategy,
};
use bagholder_data::Store;
use serde::Deserialize;
use tower_http::{cors::CorsLayer, services::ServeDir};

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
    /// Which trough to enter, counting back from most recent (0 = latest).
    /// Clamped to the available range.
    pe_index: Option<usize>,
}

fn default_strategy() -> String {
    "buy_and_hold".into()
}

/// Pull quarterly EPS out of a fundamentals set as `(period, value)`.
fn quarterly_eps(funds: &[Fundamental]) -> Vec<(chrono::NaiveDate, f64)> {
    funds
        .iter()
        .filter(|f| f.metric == "eps_basic" && f.period_type == "Q")
        .map(|f| (f.period, f.value))
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

async fn backtest(
    State(db): State<Db>,
    Query(q): Query<BacktestQuery>,
) -> Result<Json<BacktestResult>, (StatusCode, String)> {
    let strategy = match q.strategy.as_str() {
        "sma_crossover" => Strategy::SmaCrossover {
            fast: q.fast.unwrap_or(20),
            slow: q.slow.unwrap_or(50),
        },
        _ => Strategy::BuyAndHold,
    };

    let ticker = q.ticker.clone();
    let pe_min = q.entry.as_deref() == Some("pe_min");
    // Blocking DB + network I/O must not run on the async runtime's workers.
    let (bars, eps) = tokio::task::spawn_blocking(move || {
        let db = db.lock().unwrap();
        let bars = db.ohlcv(&ticker)?;
        // Only the fundamentals fetch is conditional — skip it for plain runs.
        let eps = if pe_min {
            quarterly_eps(&db.fundamentals(&ticker)?)
        } else {
            Vec::new()
        };
        Ok::<_, anyhow::Error>((bars, eps))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;

    if pe_min {
        let series = pe_series(&bars, &eps);
        let window = q.pe_window.unwrap_or(63);
        let minima = local_minima(&series, window);
        if minima.is_empty() {
            return Err((StatusCode::UNPROCESSABLE_ENTITY,
                "no P/E history to find a minimum (missing EPS?)".into()));
        }
        // Step back from the most recent trough; clamp to what's available.
        let count = minima.len();
        let k = q.pe_index.unwrap_or(0).min(count - 1);
        let (entry_date, entry_pe) = series[minima[count - 1 - k]];
        let trimmed: Vec<Bar> = bars.into_iter().filter(|b| b.date >= entry_date).collect();
        let mut result = run_portfolio_backtest(&q.ticker, &trimmed, &strategy, 10_000.0, &FillCosts::ZERO);
        result.entry_date = Some(entry_date);
        result.entry_pe = Some(entry_pe);
        result.entry_index = Some(k);
        result.entry_count = Some(count);
        Ok(Json(result))
    } else {
        Ok(Json(run_portfolio_backtest(
            &q.ticker,
            &trim_years(bars, q.years),
            &strategy,
            10_000.0,
            &FillCosts::ZERO,
        )))
    }
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

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[tokio::main]
async fn main() {
    let db: Db = Arc::new(Mutex::new(
        Store::open("bagholder.duckdb").expect("opening data store"),
    ));

    let app = Router::new()
        .route("/api/backtest", get(backtest))
        .route("/api/fundamentals", get(fundamentals))
        .route("/api/pe_history", get(pe_history_handler))
        .route("/api/screen", get(screen))
        // Serve the trunk-built frontend. Run `trunk build` in crates/web first.
        .fallback_service(ServeDir::new("crates/web/dist"))
        .layer(CorsLayer::permissive())
        .with_state(db);

    let addr = "127.0.0.1:3000";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}
