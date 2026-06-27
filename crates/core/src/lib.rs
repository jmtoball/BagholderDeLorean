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

    /// Resolve an order to a fill and apply it.
    pub fn execute(&mut self, order: &Order, price: f64, date: NaiveDate) {
        match order {
            Order::Market { ticker, qty } => self.fill(ticker, *qty, price, date),
            Order::TargetWeight { ticker, weight } => {
                let equity = self.cash + self.shares(ticker) * price;
                let delta = weight * equity / price - self.shares(ticker);
                if delta.abs() > 1e-10 {
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

/// Order intent, resolved to a Portfolio fill at execution time.
#[derive(Clone, Debug)]
pub enum Order {
    /// Transact a fixed share count (positive = buy, negative = sell).
    Market { ticker: String, qty: f64 },
    /// Rebalance ticker to `weight` fraction of current portfolio equity.
    /// ponytail: equity uses only this ticker's price — multi-asset needs a prices map (E1).
    TargetWeight { ticker: String, weight: f64 },
}

/// Built-in strategies. An enum (not a trait) so the web form can serialize a
/// choice directly. ponytail: swap to a `trait Strategy` + registry only when
/// users need to plug in their own.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Strategy {
    BuyAndHold,
    SmaCrossover { fast: usize, slow: usize },
}

impl Strategy {
    /// Target position per bar: 1.0 = fully long, 0.0 = flat. No lookahead —
    /// signal[i] uses only closes up to and including bar i.
    pub fn signals(&self, closes: &[f64]) -> Vec<f64> {
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EquityPoint {
    pub date: NaiveDate,
    pub equity: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BacktestResult {
    pub curve: Vec<EquityPoint>,
    pub metrics: Metrics,
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
/// ponytail: signals precomputed O(n) — swap to Strategy::next_signal() when A4
/// lands the trait so incremental strategies don't recompute history each bar.
pub fn run_portfolio_backtest(
    ticker: &str,
    bars: &[Bar],
    strategy: &Strategy,
    initial_cash: f64,
) -> BacktestResult {
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let signals = strategy.signals(&closes);
    let mut portfolio = Portfolio::new(initial_cash);
    let mut curve = Vec::with_capacity(bars.len());

    for i in 0..bars.len() {
        // Mark-to-market BEFORE rebalance — today's equity reflects yesterday's position.
        let eq = portfolio.cash + portfolio.shares(ticker) * closes[i];
        curve.push(EquityPoint { date: bars[i].date, equity: eq / initial_cash });

        // Rebalance to signal[i]; fills at today's close, earns tomorrow's return.
        portfolio.execute(
            &Order::TargetWeight { ticker: ticker.to_owned(), weight: signals[i] },
            closes[i],
            bars[i].date,
        );
    }

    let rets: Vec<f64> = curve.windows(2).map(|w| w[1].equity / w[0].equity - 1.0).collect();
    let metrics = compute_metrics(&curve, &rets);
    BacktestResult { curve, metrics, entry_date: None, entry_pe: None, entry_index: None, entry_count: None }
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

    let sharpe = if rets.len() > 1 {
        let n = rets.len() as f64;
        let mean = rets.iter().sum::<f64>() / n;
        let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let sd = var.sqrt();
        if sd > 0.0 {
            mean / sd * 252f64.sqrt() // annualized from daily
        } else {
            0.0
        }
    } else {
        0.0
    };

    Metrics {
        total_return,
        cagr,
        max_drawdown,
        sharpe,
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
        p.execute(&Order::Market { ticker: "X".into(), qty: 5.0 }, 100.0, d);
        assert!((p.shares("X") - 5.0).abs() < 1e-9);
        assert!((p.cash - 500.0).abs() < 1e-9);
        p.execute(&Order::Market { ticker: "X".into(), qty: -5.0 }, 120.0, d);
        assert!(p.shares("X").abs() < 1e-9);
        assert!((p.realized_pnl - 100.0).abs() < 1e-9); // 5 * (120 - 100)
    }

    #[test]
    fn order_target_weight_allocates_full_equity() {
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        let mut p = Portfolio::new(1000.0);
        p.execute(&Order::TargetWeight { ticker: "X".into(), weight: 1.0 }, 50.0, d);
        assert!((p.shares("X") - 20.0).abs() < 1e-9); // 1000 / 50
        assert!(p.cash.abs() < 1e-9);
        // reduce to 50% weight at new price
        p.execute(&Order::TargetWeight { ticker: "X".into(), weight: 0.5 }, 50.0, d);
        assert!((p.shares("X") - 10.0).abs() < 1e-9);
    }

    #[test]
    fn portfolio_backtest_parity_with_legacy() {
        let bars = vec![bar("2020-01-01", 100.0), bar("2020-01-02", 110.0), bar("2020-01-03", 121.0)];
        let legacy = run_backtest(&bars, &Strategy::BuyAndHold);
        let new = run_portfolio_backtest("X", &bars, &Strategy::BuyAndHold, 10000.0);
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
        let new = run_portfolio_backtest("X", &bars, &strategy, 1000.0);
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
