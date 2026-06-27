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
    BacktestResult { curve, metrics }
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
