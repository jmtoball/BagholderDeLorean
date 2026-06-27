//! Backtest engine: bars in, equity curve + metrics out. No I/O, no deps on
//! data sources — so it also compiles to WASM for the web crate to reuse the
//! DTOs (Bar, BacktestResult).

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bar {
    pub date: NaiveDate,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// A single fundamental fact for one reporting period (tall layout — see the
/// `fundamentals` table). `metric` is a canonical name like "revenue" or
/// "eps_basic"; `period` is the statement end date; `period_type` is "Q"
/// (quarterly) or "FY" (annual) — income-statement facts report both, so this
/// is needed to tell a quarter's revenue from the full year's.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fundamental {
    pub period: NaiveDate,
    pub metric: String,
    pub period_type: String,
    pub value: f64,
}

/// Trailing-twelve-month price/earnings from the latest close and the four most
/// recent quarterly EPS figures (most-recent-first). `None` when EPS history is
/// short or trailing earnings are non-positive (P/E is then undefined).
pub fn pe_ttm(latest_close: f64, eps_quarters_recent_first: &[f64]) -> Option<f64> {
    if eps_quarters_recent_first.len() < 4 {
        return None;
    }
    let ttm: f64 = eps_quarters_recent_first[..4].iter().sum();
    (ttm > 0.0).then_some(latest_close / ttm)
}

/// One screener hit: a company's P/E and how it sits versus its industry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub ticker: String,
    pub industry: String,
    pub pe: f64,
    pub industry_median_pe: f64,
    /// pe / industry_median_pe; < 1 means cheaper than industry peers.
    pub relative_pe: f64,
}

/// One tax lot: a batch of shares acquired at a single price.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lot {
    pub qty: f64,
    pub entry_price: f64,
    pub entry_date: NaiveDate,
}

/// Portfolio state: cash + open lots + running realized P&L.
/// ponytail: single-currency, no margin — extend for multi-currency or leverage.
#[derive(Clone, Debug, Default)]
pub struct Portfolio {
    pub cash: f64,
    /// ticker → open lots in fill order (FIFO).
    pub positions: HashMap<String, Vec<Lot>>,
    pub realized_pnl: f64,
}

impl Portfolio {
    pub fn new(cash: f64) -> Self {
        Portfolio { cash, ..Default::default() }
    }

    /// Execute a fill: positive qty = buy, negative = sell (FIFO close).
    pub fn fill(&mut self, ticker: &str, qty: f64, price: f64, date: NaiveDate) {
        if qty > 0.0 {
            self.cash -= qty * price;
            self.positions
                .entry(ticker.to_owned())
                .or_default()
                .push(Lot { qty, entry_price: price, entry_date: date });
        } else if qty < 0.0 {
            let mut remaining = -qty;
            let lots = self.positions.entry(ticker.to_owned()).or_default();
            for lot in lots.iter_mut() {
                if remaining <= 0.0 {
                    break;
                }
                let closed = lot.qty.min(remaining);
                self.realized_pnl += closed * (price - lot.entry_price);
                self.cash += closed * price;
                lot.qty -= closed;
                remaining -= closed;
            }
            lots.retain(|l| l.qty > 0.0);
        }
    }

    /// Total shares held in a position.
    pub fn shares(&self, ticker: &str) -> f64 {
        self.positions
            .get(ticker)
            .map(|lots| lots.iter().map(|l| l.qty).sum())
            .unwrap_or(0.0)
    }

    /// Cash + mark-to-market value of all open positions.
    pub fn equity(&self, prices: &HashMap<String, f64>) -> f64 {
        self.cash
            + self
                .positions
                .iter()
                .map(|(t, lots)| {
                    let p = prices.get(t).copied().unwrap_or(0.0);
                    lots.iter().map(|l| l.qty * p).sum::<f64>()
                })
                .sum::<f64>()
    }

    /// Rebalance to `alloc` target weights using total portfolio equity.
    /// Tickers absent from `prices` or with zero price are skipped.
    /// ponytail: ADV per ticker not tracked → market impact = 0; add a
    /// ticker→adv map when impact needs to be modelled for multi-asset.
    pub fn rebalance(
        &mut self,
        alloc: &Allocation,
        prices: &HashMap<String, f64>,
        date: NaiveDate,
        costs: &FillCosts,
    ) {
        let equity = self.equity(prices);
        for (ticker, &weight) in &alloc.0 {
            let price = match prices.get(ticker.as_str()) {
                Some(&p) if p > 0.0 => p,
                _ => continue,
            };
            let delta = weight * equity / price - self.shares(ticker);
            if delta.abs() > 1e-10 {
                self.cash -= costs.total(delta, price, 0.0);
                if delta < 0.0 {
                    self.cash -= costs.transaction_tax * delta.abs() * price;
                }
                self.fill(ticker, delta, price, date);
            }
        }
    }

    /// Resolve an order to a fill and apply it, deducting `costs` from cash.
    /// `adv` = average daily volume in shares (pass 0.0 when market_impact is 0).
    pub fn execute(&mut self, order: &Order, price: f64, date: NaiveDate, costs: &FillCosts, adv: f64) {
        match order {
            Order::Market { ticker, qty } => {
                self.cash -= costs.total(*qty, price, adv);
                if *qty < 0.0 {
                    self.cash -= costs.transaction_tax * qty.abs() * price;
                }
                self.fill(ticker, *qty, price, date);
            }
            Order::TargetWeight { ticker, weight } => {
                let equity = self.cash + self.shares(ticker) * price;
                let delta = weight * equity / price - self.shares(ticker);
                if delta.abs() > 1e-10 {
                    self.cash -= costs.total(delta, price, adv);
                    if delta < 0.0 {
                        self.cash -= costs.transaction_tax * delta.abs() * price;
                    }
                    self.fill(ticker, delta, price, date);
                }
            }
        }
    }

    /// Sum of unrealized gains/losses at current prices.
    pub fn unrealized_pnl(&self, prices: &HashMap<String, f64>) -> f64 {
        self.positions
            .iter()
            .map(|(t, lots)| {
                let p = prices.get(t).copied().unwrap_or(0.0);
                lots.iter().map(|l| l.qty * (p - l.entry_price)).sum::<f64>()
            })
            .sum()
    }
}

/// Multi-asset target allocation: ticker → weight fraction (should sum to ≤ 1.0).
#[derive(Clone, Debug, Default)]
pub struct Allocation(pub HashMap<String, f64>);

impl Allocation {
    /// Scale all weights so they sum to exactly 1.0.
    pub fn normalize(mut self) -> Self {
        let total: f64 = self.0.values().sum();
        if total > 0.0 {
            for w in self.0.values_mut() {
                *w /= total;
            }
        }
        self
    }
}

/// Drift bands for the 5/25 rebalancing rule (absolute + relative thresholds).
#[derive(Clone, Debug)]
pub struct BandConfig {
    /// Maximum absolute drift from target weight (e.g. 0.05 = 5 percentage points).
    pub absolute: f64,
    /// Maximum relative drift from target weight (e.g. 0.25 = 25%).
    pub relative: f64,
}

/// Controls when a rebalance is triggered.
#[derive(Clone, Debug, Default)]
pub struct RebalanceConfig {
    /// Trigger if this many calendar days have elapsed since the last rebalance.
    pub calendar_days: Option<u32>,
    /// Trigger if any position drifts beyond these bands.
    pub bands: Option<BandConfig>,
    /// If true, rebalance ALL positions when triggered; if false, only drifted ones.
    pub full: bool,
}

/// Returns true when the portfolio needs to be rebalanced according to `config`.
pub fn needs_rebalance(
    portfolio: &Portfolio,
    alloc: &Allocation,
    prices: &HashMap<String, f64>,
    config: &RebalanceConfig,
    last_rebalance: NaiveDate,
    today: NaiveDate,
) -> bool {
    if let Some(days) = config.calendar_days {
        if (today - last_rebalance).num_days() >= days as i64 {
            return true;
        }
    }
    if let Some(ref bands) = config.bands {
        let equity = portfolio.equity(prices);
        if equity <= 0.0 { return false; }
        for (ticker, &target) in &alloc.0 {
            let price = prices.get(ticker.as_str()).copied().unwrap_or(0.0);
            let actual = portfolio.shares(ticker) * price / equity;
            let drift = (actual - target).abs();
            if drift > bands.absolute || (target > 0.0 && drift / target > bands.relative) {
                return true;
            }
        }
    }
    false
}

/// Per-fill transaction costs: flat commission, proportional spread, and
/// square-root market impact (Almgren-Chriss model).
#[derive(Clone, Debug)]
pub struct FillCosts {
    /// Flat commission per order (same currency as portfolio cash).
    pub commission: f64,
    /// One-way spread cost as a fraction of notional (e.g. 0.0001 = 1 bp).
    pub spread_fraction: f64,
    /// Almgren-Chriss market-impact coefficient σ:
    ///   impact = σ × √(|qty| / adv) × price.
    /// 0.0 disables impact. Typical values: 0.1–1.0 depending on liquidity.
    pub market_impact: f64,
    /// Fraction of sell proceeds deducted as transaction tax (e.g. 0.005 = 0.5% UK stamp duty).
    /// ponytail: single rate; extend to HashMap<String, f64> for per-country rates.
    pub transaction_tax: f64,
    /// Daily borrow cost on short positions as fraction of notional (e.g. 0.0003 = 3 bp/day).
    /// Zero-ops for long-only strategies; activates when H4/H5 adds short positions.
    pub borrow_rate_daily: f64,
}

impl FillCosts {
    pub const ZERO: Self = Self {
        commission: 0.0,
        spread_fraction: 0.0,
        market_impact: 0.0,
        transaction_tax: 0.0,
        borrow_rate_daily: 0.0,
    };

    /// Total cost for a fill. `adv` = average daily volume in shares; only
    /// used when `market_impact > 0`.
    fn total(&self, qty: f64, price: f64, adv: f64) -> f64 {
        let flat = self.commission + self.spread_fraction * qty.abs() * price;
        let impact = if self.market_impact > 0.0 && adv > 0.0 {
            self.market_impact * (qty.abs() / adv).sqrt() * price
        } else {
            0.0
        };
        flat + impact
    }
}

/// Order intent, resolved to a Portfolio fill at execution time.
#[derive(Clone, Debug)]
pub enum Order {
    /// Transact a fixed share count (positive = buy, negative = sell).
    Market { ticker: String, qty: f64 },
    /// Rebalance ticker to `weight` fraction of current portfolio equity.
    /// ponytail: equity uses only this ticker's price — multi-asset needs a prices map (E1).
    TargetWeight { ticker: String, weight: f64 },
}

/// Incremental signal source: consumes one bar at a time and returns a target
/// weight (0.0 = flat, 1.0 = fully long). Implementations must not store or
/// peek at future data — the no-lookahead invariant applies here.
pub trait SignalGenerator {
    fn next(&mut self, bar: &Bar) -> f64;
}

struct BuyAndHoldGen;
impl SignalGenerator for BuyAndHoldGen {
    fn next(&mut self, _bar: &Bar) -> f64 { 1.0 }
}

/// ponytail: O(fast + slow) per bar — swap to a running-sum deque when windows
/// exceed ~200 and history exceeds ~10k bars.
struct SmaCrossoverGen { fast: usize, slow: usize, history: Vec<f64> }
impl SignalGenerator for SmaCrossoverGen {
    fn next(&mut self, bar: &Bar) -> f64 {
        self.history.push(bar.close);
        let n = self.history.len();
        if n < self.slow { return 0.0; }
        let fast_avg = self.history[n - self.fast..].iter().sum::<f64>() / self.fast as f64;
        let slow_avg = self.history[n - self.slow..].iter().sum::<f64>() / self.slow as f64;
        if fast_avg > slow_avg { 1.0 } else { 0.0 }
    }
}

/// Built-in strategies. An enum (not a trait) so the web form can serialize a
/// choice directly; `into_generator()` bridges to the incremental engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Strategy {
    BuyAndHold,
    SmaCrossover { fast: usize, slow: usize },
}

impl Strategy {
    pub fn into_generator(self) -> Box<dyn SignalGenerator> {
        match self {
            Strategy::BuyAndHold => Box::new(BuyAndHoldGen),
            Strategy::SmaCrossover { fast, slow } => {
                Box::new(SmaCrossoverGen { fast, slow, history: Vec::new() })
            }
        }
    }

    /// Batch signals — kept for `run_backtest` and the `sma_has_no_lookahead` test.
    fn signals(&self, closes: &[f64]) -> Vec<f64> {
        match *self {
            Strategy::BuyAndHold => vec![1.0; closes.len()],
            Strategy::SmaCrossover { fast, slow } => {
                let f = sma(closes, fast);
                let s = sma(closes, slow);
                (0..closes.len())
                    .map(|i| match (f[i], s[i]) {
                        (Some(a), Some(b)) if a > b => 1.0,
                        _ => 0.0,
                    })
                    .collect()
            }
        }
    }
}

/// 20-day trailing average daily volume at bar index `i` (excludes bar `i`).
/// ponytail: O(window) per call — fine for w=20; use a running sum if window grows large.
fn rolling_adv(bars: &[Bar], i: usize, window: usize) -> f64 {
    let start = i.saturating_sub(window);
    let n = i - start;
    if n == 0 { return bars.get(i).map(|b| b.volume).unwrap_or(1.0); }
    bars[start..i].iter().map(|b| b.volume).sum::<f64>() / n as f64
}

/// Rolling simple moving average; `None` until the window fills. O(n).
fn sma(xs: &[f64], window: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; xs.len()];
    if window == 0 {
        return out;
    }
    let mut sum = 0.0;
    for i in 0..xs.len() {
        sum += xs[i];
        if i >= window {
            sum -= xs[i - window];
        }
        if i + 1 >= window {
            out[i] = Some(sum / window as f64);
        }
    }
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metrics {
    pub total_return: f64,
    pub cagr: f64,
    pub max_drawdown: f64,
    pub sharpe: f64,
    /// Sortino ratio: annualized mean return / annualized downside deviation.
    /// `f64::INFINITY` when there are no negative returns.
    pub sortino: f64,
    /// Total return divided by absolute max drawdown. Undefined (0.0) when max_drawdown is 0.
    pub recovery_factor: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EquityPoint {
    pub date: NaiveDate,
    pub equity: f64,
}

/// Per-position breakdown included in a portfolio-level backtest result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PositionSummary {
    pub ticker: String,
    pub shares: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BacktestResult {
    pub curve: Vec<EquityPoint>,
    pub metrics: Metrics,
    /// Per-position breakdown; empty for legacy `run_backtest` results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<PositionSummary>,
    /// Set when the run entered at a P/E-minimum: the chosen entry date and the
    /// P/E there. `None` for ordinary runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_date: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_pe: Option<f64>,
    /// Which trough this entered, counting back from most recent (0 = latest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_index: Option<usize>,
    /// Total number of P/E troughs available to step through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_count: Option<usize>,
}

/// Close-to-close simulation. Equity starts at 1.0; each day applies
/// `yesterday's signal * today's return`, so no future data leaks in.
pub fn run_backtest(bars: &[Bar], strategy: &Strategy) -> BacktestResult {
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let signals = strategy.signals(&closes);
    let mut equity = 1.0;
    let mut curve = Vec::with_capacity(bars.len());
    let mut rets = Vec::with_capacity(bars.len().saturating_sub(1));
    for i in 0..bars.len() {
        if i > 0 {
            let pct = closes[i] / closes[i - 1] - 1.0;
            let r = signals[i - 1] * pct;
            equity *= 1.0 + r;
            rets.push(r);
        }
        curve.push(EquityPoint {
            date: bars[i].date,
            equity,
        });
    }
    let metrics = compute_metrics(&curve, &rets);
    BacktestResult {
        curve,
        metrics,
        positions: vec![],
        entry_date: None,
        entry_pe: None,
        entry_index: None,
        entry_count: None,
    }
}

/// Event-driven single-asset backtest using the portfolio state model.
/// Processes bars in chronological order; at each bar, marks to market then
/// rebalances to `signal * equity / price` shares at the bar's close.
///
/// Produces the same equity curve as `run_backtest` (verified by parity tests).
pub fn run_portfolio_backtest(
    ticker: &str,
    bars: &[Bar],
    strategy: &Strategy,
    initial_cash: f64,
    costs: &FillCosts,
    // Daily risk-free rate on idle cash. 0.0 = off. Pull from FRED DGS3MO/252 for live rate.
    rfr_daily: f64,
) -> BacktestResult {
    let mut gen = strategy.clone().into_generator();
    let mut portfolio = Portfolio::new(initial_cash);
    let mut curve = Vec::with_capacity(bars.len());

    for (i, bar) in bars.iter().enumerate() {
        // Overnight accruals from the previous bar's end-of-day state.
        // Skipped at i=0 so the opening equity is always exactly 1.0.
        if i > 0 {
            if costs.borrow_rate_daily > 0.0 {
                let short_qty = (-portfolio.shares(ticker)).max(0.0);
                portfolio.cash -= short_qty * bar.close * costs.borrow_rate_daily;
            }
            if rfr_daily != 0.0 {
                portfolio.cash += portfolio.cash.max(0.0) * rfr_daily;
            }
        }

        // Mark-to-market BEFORE rebalance — today's equity reflects yesterday's position.
        let eq = portfolio.cash + portfolio.shares(ticker) * bar.close;
        curve.push(EquityPoint { date: bar.date, equity: eq / initial_cash });

        // Signal uses only data through this bar; fills at close, earns next bar's return.
        let weight = gen.next(bar);
        let adv = rolling_adv(bars, i, 20);
        portfolio.execute(
            &Order::TargetWeight { ticker: ticker.to_owned(), weight },
            bar.close,
            bar.date,
            costs,
            adv,
        );
    }

    let rets: Vec<f64> = curve.windows(2).map(|w| w[1].equity / w[0].equity - 1.0).collect();
    let metrics = compute_metrics(&curve, &rets);

    let final_close = bars.last().map(|b| b.close).unwrap_or(0.0);
    let shares = portfolio.shares(ticker);
    let unrealized = portfolio
        .positions
        .get(ticker)
        .map(|lots| lots.iter().map(|l| l.qty * (final_close - l.entry_price)).sum())
        .unwrap_or(0.0);
    let positions = vec![PositionSummary {
        ticker: ticker.to_owned(),
        shares,
        realized_pnl: portfolio.realized_pnl,
        unrealized_pnl: unrealized,
    }];

    BacktestResult { curve, metrics, positions, entry_date: None, entry_pe: None, entry_index: None, entry_count: None }
}

/// Point-in-time trailing P/E for each bar: `close / TTM EPS`, where TTM EPS is
/// the sum of the four most recent quarterly EPS figures with period end on or
/// before the bar's date. Bars lacking four prior quarters, or with non-positive
/// trailing earnings, are skipped. `eps_q` is `(period_end, eps)` per quarter.
///
/// ponytail: uses the EPS period-end as the "known from" date — a quarter isn't
/// actually filed until weeks later, so this is mildly optimistic. Swap in the
/// SEC filing date for a strict point-in-time series.
pub fn pe_series(bars: &[Bar], eps_q: &[(NaiveDate, f64)]) -> Vec<(NaiveDate, f64)> {
    let mut eps = eps_q.to_vec();
    eps.sort_by_key(|(d, _)| *d);
    let mut out = Vec::new();
    for b in bars {
        let recent: Vec<f64> = eps
            .iter()
            .filter(|(d, _)| *d <= b.date)
            .rev()
            .take(4)
            .map(|(_, v)| *v)
            .collect();
        if recent.len() < 4 {
            continue;
        }
        let ttm: f64 = recent.iter().sum();
        if ttm > 0.0 {
            out.push((b.date, b.close / ttm));
        }
    }
    out
}

/// One point on a P/E-over-time series (used for charting).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PePoint {
    pub date: NaiveDate,
    pub pe: f64,
}

/// A P/E series plus its troughs, ready to plot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeHistory {
    pub series: Vec<PePoint>,
    pub troughs: Vec<PePoint>,
}

/// Build the P/E-over-time series and its troughs — same inputs and window as
/// `pe_series` / `local_minima`, packaged for the chart.
pub fn pe_history(bars: &[Bar], eps_q: &[(NaiveDate, f64)], window: usize) -> PeHistory {
    let s = pe_series(bars, eps_q);
    let troughs = local_minima(&s, window)
        .into_iter()
        .map(|i| PePoint {
            date: s[i].0,
            pe: s[i].1,
        })
        .collect();
    let series = s
        .into_iter()
        .map(|(date, pe)| PePoint { date, pe })
        .collect();
    PeHistory { series, troughs }
}

/// Indices of local minima (troughs) in a `(date, value)` series: a point that
/// is the smallest within a full ±`window` neighbourhood. Points within `window`
/// of either end are excluded — a trough must be *confirmed* by later data, so a
/// still-falling tail doesn't flag the last point. After each hit it skips ahead
/// by `window` so one trough isn't reported many times. Deliberately
/// retrospective (it inspects later points to confirm a trough).
pub fn local_minima(series: &[(NaiveDate, f64)], window: usize) -> Vec<usize> {
    let w = window.max(1);
    let n = series.len();
    let mut mins = Vec::new();
    let mut i = w;
    while i + w < n {
        if (i - w..=i + w).all(|j| series[i].1 <= series[j].1) {
            mins.push(i);
            i += w; // step past this trough
        } else {
            i += 1;
        }
    }
    mins
}

fn compute_metrics(curve: &[EquityPoint], rets: &[f64]) -> Metrics {
    let total_return = curve.last().map(|p| p.equity - 1.0).unwrap_or(0.0);
    let years = (curve.len().max(1) as f64) / 252.0; // ~252 trading days/year
    let cagr = if years > 0.0 {
        (1.0 + total_return).powf(1.0 / years) - 1.0
    } else {
        0.0
    };

    let mut peak = f64::MIN;
    let mut max_drawdown = 0.0;
    for p in curve {
        peak = peak.max(p.equity);
        let dd = (p.equity - peak) / peak;
        max_drawdown = f64::min(max_drawdown, dd);
    }

    let (sharpe, sortino) = if rets.len() > 1 {
        let n = rets.len() as f64;
        let mean = rets.iter().sum::<f64>() / n;
        let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let sd = var.sqrt();
        let sharpe = if sd > 0.0 { mean / sd * 252f64.sqrt() } else { 0.0 };
        // Sortino: downside deviation uses only negative excess returns (target = 0).
        let downside_var = rets.iter().map(|r| r.min(0.0).powi(2)).sum::<f64>() / n;
        let sortino = if downside_var > 0.0 {
            mean * 252f64.sqrt() / downside_var.sqrt()
        } else {
            f64::INFINITY
        };
        (sharpe, sortino)
    } else {
        (0.0, 0.0)
    };

    let recovery_factor = if max_drawdown < 0.0 {
        total_return / max_drawdown.abs()
    } else {
        0.0
    };

    Metrics {
        total_return,
        cagr,
        max_drawdown,
        sharpe,
        sortino,
        recovery_factor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(d: &str, c: f64) -> Bar {
        Bar {
            date: d.parse().unwrap(),
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 0.0,
        }
    }

    #[test]
    fn sortino_and_recovery_factor() {
        // Rising then falling: one negative return → Sortino < Sharpe; drawdown exists → recovery > 0.
        let bars = vec![
            bar("2020-01-01", 100.0),
            bar("2020-01-02", 110.0),
            bar("2020-01-03", 90.0),
        ];
        let r = run_backtest(&bars, &Strategy::BuyAndHold);
        assert!(r.metrics.sharpe.is_finite());
        assert!(r.metrics.sortino.is_finite());
        // One negative return → downside dev > 0 → Sortino is finite and positive if total > 0.
        assert!(r.metrics.max_drawdown < 0.0);
        assert!(r.metrics.recovery_factor != 0.0);
        // Monotonically rising: all returns positive → Sortino = ∞.
        let bars_up = vec![
            bar("2020-01-01", 100.0),
            bar("2020-01-02", 110.0),
            bar("2020-01-03", 121.0),
        ];
        let r_up = run_backtest(&bars_up, &Strategy::BuyAndHold);
        assert_eq!(r_up.metrics.sortino, f64::INFINITY);
        assert_eq!(r_up.metrics.recovery_factor, 0.0); // no drawdown
    }

    #[test]
    fn buy_and_hold_matches_price_ratio() {
        let bars = vec![
            bar("2020-01-01", 100.0),
            bar("2020-01-02", 110.0),
            bar("2020-01-03", 121.0),
        ];
        let r = run_backtest(&bars, &Strategy::BuyAndHold);
        assert!((r.curve.last().unwrap().equity - 1.21).abs() < 1e-9);
        assert!((r.metrics.total_return - 0.21).abs() < 1e-9);
        assert!(r.metrics.max_drawdown.abs() < 1e-9); // monotonic up => no drawdown
    }

    #[test]
    fn pe_series_is_point_in_time() {
        let eps = vec![
            ("2020-03-31".parse().unwrap(), 1.0),
            ("2020-06-30".parse().unwrap(), 1.0),
            ("2020-09-30".parse().unwrap(), 1.0),
            ("2020-12-31".parse().unwrap(), 1.0),
        ];
        let bars = vec![bar("2020-11-01", 40.0), bar("2021-01-15", 80.0)];
        let s = pe_series(&bars, &eps);
        // 2020-11-01 only knows 3 quarters -> skipped; 2021-01-15 has 4 (TTM 4).
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0, "2021-01-15".parse::<NaiveDate>().unwrap());
        assert!((s[0].1 - 20.0).abs() < 1e-9); // 80 / 4
    }

    #[test]
    fn pe_history_marks_troughs() {
        let eps: Vec<(NaiveDate, f64)> = ["2019-03-31", "2019-06-30", "2019-09-30", "2019-12-31"]
            .iter()
            .map(|d| (d.parse().unwrap(), 0.25)) // TTM EPS = 1.0 -> pe == close
            .collect();
        let closes = [5.0, 3.0, 4.0, 2.0, 6.0, 1.0, 7.0];
        let bars: Vec<Bar> = closes
            .iter()
            .enumerate()
            .map(|(i, &c)| bar(&format!("2020-01-{:02}", i + 1), c))
            .collect();
        let h = pe_history(&bars, &eps, 1);
        assert_eq!(h.series.len(), 7);
        assert!((h.series[0].pe - 5.0).abs() < 1e-9);
        let tvals: Vec<f64> = h.troughs.iter().map(|t| t.pe).collect();
        assert_eq!(tvals, vec![3.0, 2.0, 1.0]); // the troughs
    }

    #[test]
    fn local_minima_finds_troughs() {
        let d = |i: i32| chrono::NaiveDate::from_num_days_from_ce_opt(737000 + i).unwrap();
        let series: Vec<_> = [5.0, 3.0, 4.0, 2.0, 6.0, 1.0, 7.0]
            .iter()
            .enumerate()
            .map(|(i, &v)| (d(i as i32), v))
            .collect();
        let mins = local_minima(&series, 1);
        assert_eq!(mins, vec![1, 3, 5]); // values 3, 2, 1
    }

    #[test]
    fn pe_ttm_sums_four_quarters_and_guards_losses() {
        assert_eq!(pe_ttm(100.0, &[1.0, 1.0, 1.0, 1.0, 5.0]), Some(25.0)); // uses newest 4
        assert_eq!(pe_ttm(100.0, &[1.0, 1.0]), None); // too few quarters
        assert_eq!(pe_ttm(100.0, &[-1.0, -1.0, 0.0, 0.0]), None); // non-positive TTM
    }

    #[test]
    fn order_market_buy_and_sell() {
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        let mut p = Portfolio::new(1000.0);
        p.execute(&Order::Market { ticker: "X".into(), qty: 5.0 }, 100.0, d, &FillCosts::ZERO, 0.0);
        assert!((p.shares("X") - 5.0).abs() < 1e-9);
        assert!((p.cash - 500.0).abs() < 1e-9);
        p.execute(&Order::Market { ticker: "X".into(), qty: -5.0 }, 120.0, d, &FillCosts::ZERO, 0.0);
        assert!(p.shares("X").abs() < 1e-9);
        assert!((p.realized_pnl - 100.0).abs() < 1e-9); // 5 * (120 - 100)
    }

    #[test]
    fn order_target_weight_allocates_full_equity() {
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        let mut p = Portfolio::new(1000.0);
        p.execute(&Order::TargetWeight { ticker: "X".into(), weight: 1.0 }, 50.0, d, &FillCosts::ZERO, 0.0);
        assert!((p.shares("X") - 20.0).abs() < 1e-9); // 1000 / 50
        assert!(p.cash.abs() < 1e-9);
        // reduce to 50% weight at new price
        p.execute(&Order::TargetWeight { ticker: "X".into(), weight: 0.5 }, 50.0, d, &FillCosts::ZERO, 0.0);
        assert!((p.shares("X") - 10.0).abs() < 1e-9);
    }

    #[test]
    fn flat_commission_reduces_equity() {
        // Buy 10 shares @ 100 costs $10 commission → cash = -10, equity at bar 1 = 990/1000.
        let bars = vec![bar("2020-01-01", 100.0), bar("2020-01-02", 100.0)];
        let costs = FillCosts { commission: 10.0, ..FillCosts::ZERO };
        let r = run_portfolio_backtest("X", &bars, &Strategy::BuyAndHold, 1000.0, &costs, 0.0);
        assert!((r.curve.last().unwrap().equity - 0.99).abs() < 1e-9);
    }

    #[test]
    fn spread_fraction_reduces_equity() {
        // 1% spread on 10 shares @ 100 = $10 → same result as commission test.
        let bars = vec![bar("2020-01-01", 100.0), bar("2020-01-02", 100.0)];
        let costs = FillCosts { spread_fraction: 0.01, ..FillCosts::ZERO };
        let r = run_portfolio_backtest("X", &bars, &Strategy::BuyAndHold, 1000.0, &costs, 0.0);
        assert!((r.curve.last().unwrap().equity - 0.99).abs() < 1e-9);
    }

    #[test]
    fn rfr_accrues_on_idle_cash() {
        // SMA windows larger than bar count → always flat (signal=0), all cash.
        let bars: Vec<Bar> = (1..=3).map(|i| bar(&format!("2020-01-{:02}", i), 100.0)).collect();
        let strategy = Strategy::SmaCrossover { fast: 100, slow: 200 };
        let r = run_portfolio_backtest("X", &bars, &strategy, 1000.0, &FillCosts::ZERO, 0.01);
        assert!((r.curve[0].equity - 1.0).abs() < 1e-9); // bar 0 always starts at 1.0
        assert!((r.curve[1].equity - 1.01).abs() < 1e-9); // one period of RFR
        assert!((r.curve[2].equity - 1.0201).abs() < 1e-9); // compounded
    }

    #[test]
    fn needs_rebalance_calendar_and_bands() {
        let d0: NaiveDate = "2024-01-01".parse().unwrap();
        let d29 = d0 + chrono::Duration::days(29);
        let d30 = d0 + chrono::Duration::days(30);
        let mut p = Portfolio::new(10_000.0);
        let alloc = Allocation(HashMap::from([
            ("X".to_string(), 0.5),
            ("Y".to_string(), 0.5),
        ]));
        let prices = HashMap::from([("X".to_string(), 100.0), ("Y".to_string(), 100.0)]);
        p.rebalance(&alloc, &prices, d0, &FillCosts::ZERO); // 50 shares each

        let cal = RebalanceConfig { calendar_days: Some(30), bands: None, full: true };
        assert!(!needs_rebalance(&p, &alloc, &prices, &cal, d0, d29));
        assert!(needs_rebalance(&p, &alloc, &prices, &cal, d0, d30));

        // X up to 130: X weight = 50*130/(50*130+50*100) = 6500/11500 ≈ 0.565 (drift 6.5pp > 5pp)
        let prices_drifted = HashMap::from([("X".to_string(), 130.0), ("Y".to_string(), 100.0)]);
        let no_drift = HashMap::from([("X".to_string(), 102.0), ("Y".to_string(), 100.0)]);
        let bands = RebalanceConfig {
            calendar_days: None,
            bands: Some(BandConfig { absolute: 0.05, relative: 0.25 }),
            full: true,
        };
        assert!(!needs_rebalance(&p, &alloc, &no_drift, &bands, d0, d0));
        assert!(needs_rebalance(&p, &alloc, &prices_drifted, &bands, d0, d0));
    }

    #[test]
    fn allocation_normalize_and_rebalance() {
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        let mut p = Portfolio::new(10_000.0);
        let alloc = Allocation(HashMap::from([
            ("AAPL".to_string(), 0.6),
            ("MSFT".to_string(), 0.4),
        ]));
        let prices = HashMap::from([("AAPL".to_string(), 100.0), ("MSFT".to_string(), 200.0)]);
        p.rebalance(&alloc, &prices, d, &FillCosts::ZERO);
        assert!((p.shares("AAPL") - 60.0).abs() < 1e-9); // 6000 / 100
        assert!((p.shares("MSFT") - 20.0).abs() < 1e-9); // 4000 / 200
        assert!(p.cash.abs() < 1e-9);

        // normalize: 3:1 → 75%:25%
        let alloc2 = Allocation(HashMap::from([
            ("X".to_string(), 3.0),
            ("Y".to_string(), 1.0),
        ]))
        .normalize();
        assert!((alloc2.0["X"] - 0.75).abs() < 1e-9);
        assert!((alloc2.0["Y"] - 0.25).abs() < 1e-9);
    }

    #[test]
    fn transaction_tax_deducted_on_sell() {
        // Buy 10 @ 100, then sell at 100 with 1% tax → tax = 0.01 * 10 * 100 = 10.
        // After sell: cash = 10*100 (proceeds) - 10 (tax) = 990 from zero-cash start.
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        let costs = FillCosts { transaction_tax: 0.01, ..FillCosts::ZERO };
        let mut p = Portfolio::new(1000.0);
        p.execute(&Order::Market { ticker: "X".into(), qty: 10.0 }, 100.0, d, &costs, 0.0);
        p.execute(&Order::Market { ticker: "X".into(), qty: -10.0 }, 100.0, d, &costs, 0.0);
        // cash = 1000 - 1000 (buy) + 1000 (sell) - 10 (tax) = 990
        assert!((p.cash - 990.0).abs() < 1e-9);
    }

    #[test]
    fn market_impact_penalizes_large_orders() {
        // With σ=1.0 and ADV=100 shares, buying 100 shares costs 1.0 * sqrt(100/100) * price = price.
        // Buying 25 shares costs 1.0 * sqrt(25/100) * price = 0.5 * price.
        // So large order (relative to ADV) costs more impact per share.
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        let costs = FillCosts { market_impact: 1.0, ..FillCosts::ZERO };
        // 100 shares @ price 1.0, ADV=100 → impact = 1.0 * sqrt(1.0) * 1.0 = 1.0
        let impact_large = costs.total(100.0, 1.0, 100.0);
        // 25 shares @ price 1.0, ADV=100 → impact = 1.0 * sqrt(0.25) * 1.0 = 0.5
        let impact_small = costs.total(25.0, 1.0, 100.0);
        assert!(impact_large > impact_small);
        assert!((impact_large - 1.0).abs() < 1e-9);
        assert!((impact_small - 0.5).abs() < 1e-9);
        let _ = d; // suppress unused warning
    }

    #[test]
    fn portfolio_backtest_parity_with_legacy() {
        let bars = vec![bar("2020-01-01", 100.0), bar("2020-01-02", 110.0), bar("2020-01-03", 121.0)];
        let legacy = run_backtest(&bars, &Strategy::BuyAndHold);
        let new = run_portfolio_backtest("X", &bars, &Strategy::BuyAndHold, 10000.0, &FillCosts::ZERO, 0.0);
        for (l, p) in legacy.curve.iter().zip(new.curve.iter()) {
            assert!((l.equity - p.equity).abs() < 1e-9, "mismatch at {}", l.date);
        }
        assert!((legacy.metrics.total_return - new.metrics.total_return).abs() < 1e-9);
    }

    #[test]
    fn portfolio_backtest_no_lookahead_sma() {
        // SMA crossover via portfolio loop must match the scalar loop (which the
        // sma_has_no_lookahead test already proves is signal-clean).
        let bars: Vec<Bar> = (1..=10)
            .map(|i| bar(&format!("2020-01-{:02}", i), (i * 10) as f64))
            .collect();
        let strategy = Strategy::SmaCrossover { fast: 2, slow: 4 };
        let legacy = run_backtest(&bars, &strategy);
        let new = run_portfolio_backtest("X", &bars, &strategy, 1000.0, &FillCosts::ZERO, 0.0);
        for (l, p) in legacy.curve.iter().zip(new.curve.iter()) {
            assert!((l.equity - p.equity).abs() < 1e-9, "SMA mismatch at {}", l.date);
        }
    }

    #[test]
    fn portfolio_equity_and_unrealized_pnl() {
        let mut p = Portfolio::new(1000.0);
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        p.fill("AAPL", 10.0, 100.0, d);
        assert!(p.cash.abs() < 1e-9);
        let prices = HashMap::from([("AAPL".to_string(), 120.0)]);
        assert!((p.equity(&prices) - 1200.0).abs() < 1e-9);
        assert!((p.unrealized_pnl(&prices) - 200.0).abs() < 1e-9);
    }

    #[test]
    fn portfolio_fifo_realized_pnl() {
        let mut p = Portfolio::new(2000.0);
        let (d1, d2, d3) = (
            "2024-01-01".parse::<NaiveDate>().unwrap(),
            "2024-02-01".parse::<NaiveDate>().unwrap(),
            "2024-03-01".parse::<NaiveDate>().unwrap(),
        );
        p.fill("AAPL", 5.0, 100.0, d1); // lot 1: 5@100
        p.fill("AAPL", 5.0, 110.0, d2); // lot 2: 5@110
        // sell 7: close 5@100 (gain 150) + 2@110 (gain 40) = 190
        p.fill("AAPL", -7.0, 130.0, d3);
        assert!((p.realized_pnl - 190.0).abs() < 1e-9);
        assert!((p.shares("AAPL") - 3.0).abs() < 1e-9); // 3 remain from lot 2
    }

    #[test]
    fn sma_has_no_lookahead() {
        let closes: Vec<f64> = (1..=10).map(|x| x as f64).collect();
        let sig = Strategy::SmaCrossover { fast: 2, slow: 4 }.signals(&closes);
        // flat until the slow window fills...
        assert_eq!(sig[0], 0.0);
        assert_eq!(sig[2], 0.0);
        // ...then long on a rising series (fast SMA above slow SMA)
        assert_eq!(sig[3], 1.0);
    }
}
