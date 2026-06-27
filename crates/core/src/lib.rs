//! Backtest engine: bars in, equity curve + metrics out. No I/O, no deps on
//! data sources — so it also compiles to WASM for the web crate to reuse the
//! DTOs (Bar, BacktestResult).

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

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
    }
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
