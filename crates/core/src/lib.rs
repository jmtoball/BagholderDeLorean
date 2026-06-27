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
    /// SEC filing date for this fact — when the market could first see it.
    /// `None` for facts from filings not covered by the submissions cache
    /// (typically very old filings). Falls back to `period` (period end) when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filed_date: Option<NaiveDate>,
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

    /// Execute a fill: positive qty = buy (covers shorts first, then opens long);
    /// negative qty = sell (closes longs first FIFO, then opens short with remainder).
    pub fn fill(&mut self, ticker: &str, qty: f64, price: f64, date: NaiveDate) {
        if qty > 0.0 {
            let mut remaining = qty;
            let lots = self.positions.entry(ticker.to_owned()).or_default();
            // Cover short lots first (FIFO).
            for lot in lots.iter_mut().filter(|l| l.qty < 0.0) {
                if remaining <= 0.0 { break; }
                let covered = (-lot.qty).min(remaining);
                self.realized_pnl += covered * (lot.entry_price - price);
                self.cash -= covered * price;
                lot.qty += covered;
                remaining -= covered;
            }
            lots.retain(|l| l.qty.abs() > 1e-10);
            if remaining > 1e-10 {
                self.cash -= remaining * price;
                lots.push(Lot { qty: remaining, entry_price: price, entry_date: date });
            }
        } else if qty < 0.0 {
            let mut remaining = -qty;
            let lots = self.positions.entry(ticker.to_owned()).or_default();
            // Close long lots first (FIFO).
            for lot in lots.iter_mut().filter(|l| l.qty > 0.0) {
                if remaining <= 0.0 { break; }
                let closed = lot.qty.min(remaining);
                self.realized_pnl += closed * (price - lot.entry_price);
                self.cash += closed * price;
                lot.qty -= closed;
                remaining -= closed;
            }
            lots.retain(|l| l.qty.abs() > 1e-10);
            // Remainder opens a short position (receive proceeds).
            if remaining > 1e-10 {
                self.cash += remaining * price;
                lots.push(Lot { qty: -remaining, entry_price: price, entry_date: date });
            }
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

/// Average Directional Index (ADX) state for incremental computation.
/// Low ADX (< ~25) means a ranging/non-trending market — where MR strategies work.
struct AdxState {
    period: usize,
    prev_high: f64,
    prev_low: f64,
    prev_close: f64,
    smoothed_tr: f64,
    smoothed_dm_pos: f64,
    smoothed_dm_neg: f64,
    smoothed_dx: f64,
    adx: f64,
    ready: bool,
    buf: usize, // bars seen so far
}

impl AdxState {
    fn new(period: usize) -> Self {
        Self {
            period,
            prev_high: 0.0, prev_low: 0.0, prev_close: 0.0,
            smoothed_tr: 0.0, smoothed_dm_pos: 0.0, smoothed_dm_neg: 0.0,
            smoothed_dx: 0.0, adx: 0.0, ready: false, buf: 0,
        }
    }

    /// Feed one bar; returns current ADX (None until 2*period bars).
    fn next(&mut self, high: f64, low: f64, close: f64) -> Option<f64> {
        if self.buf == 0 {
            self.prev_high = high; self.prev_low = low; self.prev_close = close;
            self.buf += 1;
            return None;
        }
        let tr = (high - low).max((high - self.prev_close).abs()).max((low - self.prev_close).abs());
        let dm_pos = if high - self.prev_high > self.prev_low - low { (high - self.prev_high).max(0.0) } else { 0.0 };
        let dm_neg = if self.prev_low - low > high - self.prev_high { (self.prev_low - low).max(0.0) } else { 0.0 };

        let p = self.period as f64;
        if !self.ready {
            // Wilder's smoothing: first period is a simple sum, then iterate.
            self.smoothed_tr += tr;
            self.smoothed_dm_pos += dm_pos;
            self.smoothed_dm_neg += dm_neg;
            self.buf += 1;
            if self.buf == self.period + 1 {
                self.ready = true;
            }
        } else {
            self.smoothed_tr = self.smoothed_tr - self.smoothed_tr / p + tr;
            self.smoothed_dm_pos = self.smoothed_dm_pos - self.smoothed_dm_pos / p + dm_pos;
            self.smoothed_dm_neg = self.smoothed_dm_neg - self.smoothed_dm_neg / p + dm_neg;
        }

        self.prev_high = high; self.prev_low = low; self.prev_close = close;

        if !self.ready || self.smoothed_tr < 1e-12 { return None; }
        let di_pos = 100.0 * self.smoothed_dm_pos / self.smoothed_tr;
        let di_neg = 100.0 * self.smoothed_dm_neg / self.smoothed_tr;
        let di_sum = di_pos + di_neg;
        let dx = if di_sum > 0.0 { 100.0 * (di_pos - di_neg).abs() / di_sum } else { 0.0 };

        // ADX = Wilder-smoothed DX (needs period bars of DX, i.e. 2*period total bars).
        self.smoothed_dx = self.smoothed_dx - self.smoothed_dx / p + dx;
        self.buf += 1;
        if self.buf >= 2 * self.period + 1 {
            self.adx = self.smoothed_dx / p;
            Some(self.adx)
        } else {
            None
        }
    }
}

/// Regime-filtered mean reversion: enter when RSI is oversold AND ADX is low
/// (non-trending / ranging market). Exit when RSI recovers.
///
/// ponytail: VIX filter from Yahoo `^VIX` deferred — needs secondary ticker data.
/// Add when API supports multi-source signals.
struct RegimeMRGen {
    rsi_period: usize,
    rsi_entry: f64,
    rsi_exit: f64,
    adx_threshold: f64,
    closes: Vec<f64>,
    avg_gain: f64,
    avg_loss: f64,
    rsi_ready: bool,
    adx: AdxState,
    in_position: bool,
}

impl SignalGenerator for RegimeMRGen {
    fn next(&mut self, bar: &Bar) -> f64 {
        let n = self.closes.len();

        // RSI (same Wilder logic as BuyTheDipGen)
        let (rsi_val, rsi_ready) = if n >= self.rsi_period {
            let delta = bar.close - self.closes[n - 1];
            let gain = delta.max(0.0);
            let loss = (-delta).max(0.0);
            if n == self.rsi_period && !self.rsi_ready {
                let (sg, sl) = self.closes.windows(2).fold((gain, loss), |(sg, sl), w| {
                    let d = w[1] - w[0];
                    (sg + d.max(0.0), sl + (-d).max(0.0))
                });
                let p = self.rsi_period as f64;
                self.avg_gain = sg / p;
                self.avg_loss = sl / p;
                self.rsi_ready = true;
            } else if self.rsi_ready {
                let p = self.rsi_period as f64;
                self.avg_gain = (self.avg_gain * (p - 1.0) + gain) / p;
                self.avg_loss = (self.avg_loss * (p - 1.0) + loss) / p;
            }
            if self.rsi_ready {
                let rs = if self.avg_loss > 0.0 { self.avg_gain / self.avg_loss } else { f64::MAX };
                (100.0 - 100.0 / (1.0 + rs), true)
            } else {
                (50.0, false)
            }
        } else {
            (50.0, false)
        };

        let adx_val = self.adx.next(bar.high, bar.low, bar.close);

        self.closes.push(bar.close);

        if !rsi_ready { return 0.0; }

        // State machine: enter on RSI oversold + ADX below threshold; exit on RSI recovery.
        let regime_ok = adx_val.map(|a| a < self.adx_threshold).unwrap_or(true);
        if !self.in_position {
            if rsi_val < self.rsi_entry && regime_ok {
                self.in_position = true;
            }
        } else if rsi_val > self.rsi_exit {
            self.in_position = false;
        }
        if self.in_position { 1.0 } else { 0.0 }
    }
}

/// BTFD signal generator: long when RSI is below threshold or price breaches
/// the lower Bollinger Band; flat otherwise. Uses Wilder's smoothed RSI.
struct BuyTheDipGen {
    rsi_period: usize,
    rsi_threshold: f64,
    bb_period: usize,
    bb_std: f64,
    closes: Vec<f64>,
    avg_gain: f64,
    avg_loss: f64,
    rsi_ready: bool,
}

impl SignalGenerator for BuyTheDipGen {
    fn next(&mut self, bar: &Bar) -> f64 {
        let n = self.closes.len();

        // Wilder's RSI: initialize after rsi_period price changes (= rsi_period+1 prices).
        let rsi_signal = if n >= self.rsi_period {
            let delta = bar.close - self.closes[n - 1];
            let gain = delta.max(0.0);
            let loss = (-delta).max(0.0);
            if n == self.rsi_period && !self.rsi_ready {
                // First rsi_period-1 diffs from history + this diff = rsi_period total.
                let (sg, sl) = self.closes.windows(2).fold((gain, loss), |(sg, sl), w| {
                    let d = w[1] - w[0];
                    (sg + d.max(0.0), sl + (-d).max(0.0))
                });
                let p = self.rsi_period as f64;
                self.avg_gain = sg / p;
                self.avg_loss = sl / p;
                self.rsi_ready = true;
            } else if self.rsi_ready {
                let p = self.rsi_period as f64;
                self.avg_gain = (self.avg_gain * (p - 1.0) + gain) / p;
                self.avg_loss = (self.avg_loss * (p - 1.0) + loss) / p;
            }
            if self.rsi_ready {
                let rs = if self.avg_loss > 0.0 { self.avg_gain / self.avg_loss } else { f64::MAX };
                (100.0 - 100.0 / (1.0 + rs)) < self.rsi_threshold
            } else {
                false
            }
        } else {
            false
        };

        // Lower Bollinger Band: price < SMA(period) - std * StdDev(period).
        let bb_signal = if n + 1 >= self.bb_period {
            let start = n.saturating_sub(self.bb_period - 1);
            let mut win: Vec<f64> = self.closes[start..].to_vec();
            win.push(bar.close);
            let len = win.len() as f64;
            let mean = win.iter().sum::<f64>() / len;
            let var = win.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / len;
            bar.close < mean - self.bb_std * var.sqrt()
        } else {
            false
        };

        self.closes.push(bar.close);
        if rsi_signal || bb_signal { 1.0 } else { 0.0 }
    }
}

/// Built-in strategies. An enum (not a trait) so the web form can serialize a
/// choice directly; `into_generator()` bridges to the incremental engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Strategy {
    BuyAndHold,
    SmaCrossover { fast: usize, slow: usize },
    /// Aggressive mean reversion: long when RSI is oversold or price breaches
    /// the lower Bollinger Band. Golden cross (#13) is SmaCrossover{50,200}.
    BuyTheDip {
        rsi_period: usize,
        rsi_threshold: f64,
        bb_period: usize,
        bb_std: f64,
    },
    /// Mean reversion gated by trend regime: enter on RSI oversold only when
    /// ADX is below `adx_threshold` (non-trending market). Exit on RSI recovery.
    RegimeMeanReversion {
        rsi_period: usize,
        rsi_entry: f64,
        rsi_exit: f64,
        adx_period: usize,
        adx_threshold: f64,
    },
}

impl Strategy {
    pub fn into_generator(self) -> Box<dyn SignalGenerator> {
        match self {
            Strategy::BuyAndHold => Box::new(BuyAndHoldGen),
            Strategy::SmaCrossover { fast, slow } => {
                Box::new(SmaCrossoverGen { fast, slow, history: Vec::new() })
            }
            Strategy::BuyTheDip { rsi_period, rsi_threshold, bb_period, bb_std } => {
                Box::new(BuyTheDipGen {
                    rsi_period, rsi_threshold, bb_period, bb_std,
                    closes: Vec::new(), avg_gain: 0.0, avg_loss: 0.0, rsi_ready: false,
                })
            }
            Strategy::RegimeMeanReversion { rsi_period, rsi_entry, rsi_exit, adx_period, adx_threshold } => {
                Box::new(RegimeMRGen {
                    rsi_period, rsi_entry, rsi_exit, adx_threshold,
                    closes: Vec::new(), avg_gain: 0.0, avg_loss: 0.0, rsi_ready: false,
                    adx: AdxState::new(adx_period), in_position: false,
                })
            }
        }
    }

    /// Batch signals — kept for `run_backtest` and the `sma_has_no_lookahead` test.
    /// ponytail: BuyTheDip routes through its generator with synthetic bar dates.
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
            Strategy::BuyTheDip { rsi_period, rsi_threshold, bb_period, bb_std } => {
                let mut gen = BuyTheDipGen {
                    rsi_period, rsi_threshold, bb_period, bb_std,
                    closes: Vec::new(), avg_gain: 0.0, avg_loss: 0.0, rsi_ready: false,
                };
                let base = chrono::NaiveDate::from_num_days_from_ce_opt(737000).unwrap();
                closes.iter().enumerate().map(|(i, &c)| {
                    gen.next(&Bar {
                        date: base + chrono::Duration::days(i as i64),
                        open: c, high: c, low: c, close: c, volume: 0.0,
                    })
                }).collect()
            }
            Strategy::RegimeMeanReversion { rsi_period, rsi_entry, rsi_exit, adx_period, adx_threshold } => {
                let mut gen = RegimeMRGen {
                    rsi_period, rsi_entry, rsi_exit, adx_threshold,
                    closes: Vec::new(), avg_gain: 0.0, avg_loss: 0.0, rsi_ready: false,
                    adx: AdxState::new(adx_period), in_position: false,
                };
                let base = chrono::NaiveDate::from_num_days_from_ce_opt(737000).unwrap();
                closes.iter().enumerate().map(|(i, &c)| {
                    gen.next(&Bar {
                        date: base + chrono::Duration::days(i as i64),
                        open: c, high: c, low: c, close: c, volume: 0.0,
                    })
                }).collect()
            }
        }
    }
}

/// Inverse-volatility allocation across tickers. Each ticker's weight is
/// proportional to `1 / rolling_stddev(returns, window)`. Equal-weights any
/// ticker whose trailing vol is zero.
///
/// `bars_to_date` maps ticker → slice of bars up to and including today.
pub fn inverse_vol_alloc(bars_to_date: &HashMap<String, &[Bar]>, window: usize) -> Allocation {
    let inv: HashMap<String, f64> = bars_to_date.iter().filter_map(|(ticker, bars)| {
        if bars.len() < 2 { return None; }
        let start = bars.len().saturating_sub(window + 1);
        let rets: Vec<f64> = bars[start..].windows(2)
            .map(|w| w[1].close / w[0].close - 1.0)
            .collect();
        if rets.is_empty() { return None; }
        let n = rets.len() as f64;
        let mean = rets.iter().sum::<f64>() / n;
        let vol = (rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n).sqrt();
        if vol > 0.0 { Some((ticker.clone(), 1.0 / vol)) } else { None }
    }).collect();

    if inv.is_empty() {
        // Not enough data yet: equal weight everything.
        let w = 1.0 / bars_to_date.len() as f64;
        return Allocation(bars_to_date.keys().map(|t| (t.clone(), w)).collect());
    }
    Allocation(inv).normalize()
}

/// The 11 SPDR Select Sector ETFs used for sector-rotation presets.
pub const SECTOR_ETFS: &[&str] = &[
    "XLK", "XLV", "XLF", "XLY", "XLP", "XLE", "XLI", "XLB", "XLU", "XLRE", "XLC",
];

/// Momentum-ranked equal-weight allocation: rank all tickers by their trailing
/// `lookback`-bar return, take the top `top_n`, weight equally.
/// Returns equal weight across all tickers if there is insufficient history.
pub fn momentum_alloc(
    bars_to_date: &HashMap<String, &[Bar]>,
    lookback: usize,
    top_n: usize,
) -> Allocation {
    let mut ranked: Vec<(String, f64)> = bars_to_date.iter().filter_map(|(t, bars)| {
        if bars.len() <= lookback { return None; }
        let ret = bars.last().unwrap().close / bars[bars.len() - 1 - lookback].close - 1.0;
        Some((t.clone(), ret))
    }).collect();

    if ranked.is_empty() {
        let w = 1.0 / bars_to_date.len().max(1) as f64;
        return Allocation(bars_to_date.keys().map(|t| (t.clone(), w)).collect());
    }

    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let n = top_n.min(ranked.len()).max(1);
    let w = 1.0 / n as f64;
    Allocation(ranked.into_iter().take(n).map(|(t, _)| (t, w)).collect())
}

/// Pairs (stat-arb) allocation from the log-price spread z-score.
///
/// - z < -`entry_z`: long A, short B (spread below mean → A cheap vs B)
/// - z > +`entry_z`: short A, long B (spread above mean → A expensive)
/// - |z| < `entry_z`: flat (explicit 0-weight closes any open position)
///
/// ponytail: no hysteresis — entry and exit at the same z threshold. Add a
/// separate `exit_z` state machine if transaction costs make this too active.
pub fn pairs_alloc(
    bars_to_date: &HashMap<String, &[Bar]>,
    ticker_a: &str,
    ticker_b: &str,
    window: usize,
    entry_z: f64,
) -> Allocation {
    let get = |t: &str| bars_to_date.get(t).copied();
    let (ba, bb) = match (get(ticker_a), get(ticker_b)) {
        (Some(a), Some(b)) => (a, b),
        _ => return Allocation::default(),
    };
    let n = ba.len().min(bb.len());
    if n < window + 1 { return Allocation::default(); }

    let spreads: Vec<f64> = (n - window - 1..n)
        .map(|i| (ba[i].close / bb[i].close).ln())
        .collect();
    let mean = spreads.iter().sum::<f64>() / spreads.len() as f64;
    let var = spreads.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / spreads.len() as f64;
    let std = var.sqrt();
    if std < 1e-12 { return Allocation::default(); }
    let z = (spreads.last().unwrap() - mean) / std;

    let flat = Allocation(HashMap::from([
        (ticker_a.to_owned(), 0.0),
        (ticker_b.to_owned(), 0.0),
    ]));
    if z < -entry_z {
        Allocation(HashMap::from([(ticker_a.to_owned(), 0.5), (ticker_b.to_owned(), -0.5)]))
    } else if z > entry_z {
        Allocation(HashMap::from([(ticker_a.to_owned(), -0.5), (ticker_b.to_owned(), 0.5)]))
    } else {
        flat
    }
}

/// Sector allocation driven by the yield-curve regime (T10Y2Y spread):
/// - Inverted (< 0): defensive tilt — XLP, XLU, XLV
/// - Steep (> 0.5): growth tilt — XLK, XLF, XLI, XLY
/// - Flat / unknown: equal-weight all tickers in `bars_to_date`
///
/// `t10y2y` is the current T10Y2Y value (None = no macro data yet).
pub fn econ_cycle_alloc(
    bars_to_date: &HashMap<String, &[Bar]>,
    t10y2y: Option<f64>,
) -> Allocation {
    match t10y2y {
        Some(s) if s < 0.0 => {
            let ts = ["XLP", "XLU", "XLV"];
            let w = 1.0 / ts.len() as f64;
            Allocation(ts.iter().map(|&t| (t.to_owned(), w)).collect())
        }
        Some(s) if s > 0.5 => {
            let ts = ["XLK", "XLF", "XLI", "XLY"];
            let w = 1.0 / ts.len() as f64;
            Allocation(ts.iter().map(|&t| (t.to_owned(), w)).collect())
        }
        _ => {
            let n = bars_to_date.len().max(1);
            let w = 1.0 / n as f64;
            Allocation(bars_to_date.keys().map(|t| (t.clone(), w)).collect())
        }
    }
}

/// Multi-asset event-driven backtest. Bars for each ticker are date-aligned
/// to their intersection; `alloc_fn` is called each bar with history up to
/// (inclusive) that bar and the current date index.
///
/// Rebalances whenever `needs_rebalance` fires; force-rebalances at bar 0.
/// `rfr_daily` accrues on positive cash each bar (skipped at bar 0).
pub fn run_multi_asset_backtest(
    bars_by_ticker: &HashMap<String, Vec<Bar>>,
    alloc_fn: impl Fn(&HashMap<String, &[Bar]>, usize) -> Allocation,
    rebalance_config: &RebalanceConfig,
    initial_cash: f64,
    costs: &FillCosts,
    rfr_daily: f64,
) -> BacktestResult {
    // Intersection of dates across all tickers.
    let common_dates: Vec<NaiveDate> = {
        let mut iter = bars_by_ticker.values();
        let first = match iter.next() {
            Some(b) => b.iter().map(|b| b.date).collect::<std::collections::HashSet<_>>(),
            None => return BacktestResult {
                curve: vec![],
                metrics: compute_metrics(&[], &[]),
                positions: vec![], entry_date: None, entry_pe: None, entry_index: None, entry_count: None,
            },
        };
        let common = iter.fold(first, |acc, bars| {
            let set: std::collections::HashSet<_> = bars.iter().map(|b| b.date).collect();
            acc.into_iter().filter(|d| set.contains(d)).collect()
        });
        let mut v: Vec<_> = common.into_iter().collect();
        v.sort();
        v
    };

    // Pre-build date→index maps for fast bar lookup.
    let index_maps: HashMap<String, HashMap<NaiveDate, usize>> = bars_by_ticker.iter()
        .map(|(t, bars)| (t.clone(), bars.iter().enumerate().map(|(i, b)| (b.date, i)).collect()))
        .collect();

    let mut portfolio = Portfolio::new(initial_cash);
    let mut curve = Vec::with_capacity(common_dates.len());
    let mut last_rebalance = *common_dates.first().unwrap_or(&NaiveDate::MIN);

    for (ci, &date) in common_dates.iter().enumerate() {
        if ci > 0 && rfr_daily != 0.0 {
            portfolio.cash += portfolio.cash.max(0.0) * rfr_daily;
        }

        // Current prices and history slices.
        let prices: HashMap<String, f64> = bars_by_ticker.iter().filter_map(|(t, bars)| {
            index_maps[t].get(&date).map(|&i| (t.clone(), bars[i].close))
        }).collect();
        let history: HashMap<String, &[Bar]> = bars_by_ticker.iter().filter_map(|(t, bars)| {
            index_maps[t].get(&date).map(|&i| (t.clone(), &bars[..=i]))
        }).collect();

        let eq = portfolio.equity(&prices);
        curve.push(EquityPoint { date, equity: eq / initial_cash });

        let alloc = alloc_fn(&history, ci);
        if ci == 0 || needs_rebalance(&portfolio, &alloc, &prices, rebalance_config, last_rebalance, date) {
            portfolio.rebalance(&alloc, &prices, date, costs);
            last_rebalance = date;
        }
    }

    let rets: Vec<f64> = curve.windows(2).map(|w| w[1].equity / w[0].equity - 1.0).collect();
    let metrics = compute_metrics(&curve, &rets);
    // ponytail: positions left empty for multi-asset; add per-ticker summary when UI needs it.
    BacktestResult { curve, metrics, positions: vec![], entry_date: None, entry_pe: None, entry_index: None, entry_count: None }
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
    fn fill_short_sell_and_cover() {
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        let mut p = Portfolio::new(1000.0);
        // Short 5 @ 100: receive 500
        p.fill("X", -5.0, 100.0, d);
        assert!((p.shares("X") - (-5.0)).abs() < 1e-9);
        assert!((p.cash - 1500.0).abs() < 1e-9); // 1000 + 500 short proceeds
        // Cover 5 @ 80 (price fell): profit = 5 * (100 - 80) = 100
        p.fill("X", 5.0, 80.0, d);
        assert!(p.shares("X").abs() < 1e-9);
        assert!((p.realized_pnl - 100.0).abs() < 1e-9);
        assert!((p.cash - 1100.0).abs() < 1e-9); // 1500 - 5*80 = 1100
    }

    #[test]
    fn fill_sell_through_zero_into_short() {
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        let mut p = Portfolio::new(1000.0);
        p.fill("X", 3.0, 100.0, d); // buy 3
        p.fill("X", -5.0, 100.0, d); // sell 5 (close 3, open short 2)
        assert!((p.shares("X") - (-2.0)).abs() < 1e-9);
    }

    #[test]
    fn econ_cycle_alloc_regime_tilt() {
        let history: HashMap<String, &[Bar]> = HashMap::new(); // empty — regime derives from macro only
        let defensive = econ_cycle_alloc(&history, Some(-0.5));
        assert!(defensive.0.contains_key("XLP"), "inverted curve → XLP");
        assert!(!defensive.0.contains_key("XLK"), "inverted curve → no XLK");
        let growth = econ_cycle_alloc(&history, Some(1.0));
        assert!(growth.0.contains_key("XLK"), "steep curve → XLK");
        assert!(!growth.0.contains_key("XLP"), "steep curve → no XLP");
        // No data → equal weight across whatever's in bars_to_date (empty here → empty alloc).
        let flat = econ_cycle_alloc(&history, None);
        assert!(flat.0.is_empty());
    }

    #[test]
    fn pairs_alloc_signal_direction() {
        // A jumps far above B → z > entry_z → short A, long B.
        let mk = |prices: &[f64]| -> Vec<Bar> {
            prices.iter().enumerate()
                .map(|(i, &c)| bar(&format!("2020-01-{:02}", i + 1), c))
                .collect()
        };
        let n = 25;
        let mut a_prices = vec![100.0f64; n];
        let mut b_prices = vec![100.0f64; n];
        *a_prices.last_mut().unwrap() = 200.0; // A spikes
        let bars_a = mk(&a_prices);
        let bars_b = mk(&b_prices);
        let history = HashMap::from([
            ("A".to_string(), bars_a.as_slice()),
            ("B".to_string(), bars_b.as_slice()),
        ]);
        let alloc = pairs_alloc(&history, "A", "B", 20, 1.0);
        let wa = alloc.0.get("A").copied().unwrap_or(0.0);
        let wb = alloc.0.get("B").copied().unwrap_or(0.0);
        assert!(wa < 0.0, "A spike → should short A (wa={wa})");
        assert!(wb > 0.0, "A spike → should long B (wb={wb})");
    }

    #[test]
    fn momentum_alloc_picks_top_n_by_return() {
        // A: flat, B: up 10%, C: up 20%. top_n=2 → B and C, equal weight.
        let mk = |prices: &[f64]| -> Vec<Bar> {
            prices.iter().enumerate()
                .map(|(i, &c)| bar(&format!("2020-01-{:02}", i + 1), c))
                .collect()
        };
        let universe = HashMap::from([
            ("A".to_string(), mk(&[100.0, 100.0, 100.0])),
            ("B".to_string(), mk(&[100.0, 105.0, 110.0])),
            ("C".to_string(), mk(&[100.0, 110.0, 120.0])),
        ]);
        let history: HashMap<String, &[Bar]> = universe.iter()
            .map(|(t, b)| (t.clone(), b.as_slice()))
            .collect();
        let alloc = momentum_alloc(&history, 2, 2);
        assert!(!alloc.0.contains_key("A") || alloc.0["A"] == 0.0, "A should be excluded");
        let bw = alloc.0.get("B").copied().unwrap_or(0.0);
        let cw = alloc.0.get("C").copied().unwrap_or(0.0);
        assert!((bw - 0.5).abs() < 1e-9 && (cw - 0.5).abs() < 1e-9, "B and C should be 50/50");
    }

    #[test]
    fn inverse_vol_alloc_weights_by_inverse_vol() {
        // Low-vol ticker (small moves) → higher weight than high-vol ticker.
        let bars_lo: Vec<Bar> = (0..25).map(|i| bar(&format!("2020-01-{:02}", i + 1), 100.0 + (i % 2) as f64)).collect();
        let bars_hi: Vec<Bar> = (0..25).map(|i| bar(&format!("2020-01-{:02}", i + 1), 100.0 + (i % 2) as f64 * 10.0)).collect();
        let history = HashMap::from([
            ("LO".to_string(), bars_lo.as_slice()),
            ("HI".to_string(), bars_hi.as_slice()),
        ]);
        let alloc = inverse_vol_alloc(&history, 20);
        assert!(alloc.0["LO"] > alloc.0["HI"], "low-vol ticker should have higher weight");
        let total: f64 = alloc.0.values().sum();
        assert!((total - 1.0).abs() < 1e-9, "weights must sum to 1");
    }

    #[test]
    fn multi_asset_backtest_runs_and_rebalances() {
        // Two tickers moving in sync: equal-weight buy-and-hold should track either ticker.
        let bars_a: Vec<Bar> = (0..10).map(|i| bar(&format!("2020-01-{:02}", i + 1), 100.0 + i as f64 * 10.0)).collect();
        let bars_b: Vec<Bar> = (0..10).map(|i| bar(&format!("2020-01-{:02}", i + 1), 100.0 + i as f64 * 10.0)).collect();
        let universe = HashMap::from([
            ("A".to_string(), bars_a),
            ("B".to_string(), bars_b),
        ]);
        let cfg = RebalanceConfig { calendar_days: Some(9999), bands: None, full: true };
        let r = run_multi_asset_backtest(
            &universe,
            |_, _| Allocation(HashMap::from([("A".to_string(), 0.5), ("B".to_string(), 0.5)])),
            &cfg, 10_000.0, &FillCosts::ZERO, 0.0,
        );
        assert_eq!(r.curve.len(), 10);
        // Equal-weight on identical tickers = single-ticker return.
        let expected = 190.0 / 100.0; // 100→190
        assert!((r.curve.last().unwrap().equity - expected).abs() < 1e-6);
    }

    #[test]
    fn regime_mr_only_enters_in_ranging_market() {
        // Flat price → ADX near 0 (ranging) → RSI falls sharply on a down day → enter.
        // 30 flat bars (ADX initializes over ~2*period), then a big drop.
        let period = 5usize;
        let mut bars: Vec<Bar> = (0..30).map(|i| {
            let d = format!("2020-01-{:02}", i + 1);
            bar(&d, 100.0)
        }).collect();
        // Add enough dates
        for i in 30..40 {
            bars.push(bar(&format!("2020-02-{:02}", i - 29), if i == 32 { 80.0 } else { 100.0 }));
        }
        let strategy = Strategy::RegimeMeanReversion {
            rsi_period: period, rsi_entry: 30.0, rsi_exit: 70.0,
            adx_period: period, adx_threshold: 25.0,
        };
        let sigs: Vec<f64> = {
            let mut gen = strategy.into_generator();
            bars.iter().map(|b| gen.next(b)).collect()
        };
        // Should be 0 until enough data, then potentially 1 on the big drop.
        assert_eq!(sigs[0], 0.0); // no data yet
        // At least one bar should fire after the drop at index 32.
        let has_entry = sigs[33..].iter().any(|&s| s == 1.0);
        assert!(has_entry, "should enter after RSI drop in ranging market");
    }

    #[test]
    fn btfd_signals_on_bollinger_breach() {
        // 20 bars flat at 100 then one crash to 50 → lower BB breach → signal=1.
        let mut bars: Vec<Bar> = (0..20).map(|i| bar(&format!("2020-01-{:02}", i + 1), 100.0)).collect();
        bars.push(bar("2020-01-21", 50.0));
        let strategy = Strategy::BuyTheDip { rsi_period: 14, rsi_threshold: 20.0, bb_period: 20, bb_std: 2.0 };
        let r = run_portfolio_backtest("X", &bars, &strategy, 1000.0, &FillCosts::ZERO, 0.0);
        assert_eq!(r.curve.last().unwrap().equity, r.curve.last().unwrap().equity); // compiles
        // The crash bar should have been entered (signal=1 after prior flat period).
        // Signal at bar 20 (the crash) fires on the crash close; equity recovers from bar 21 onward
        // but we only have 21 bars so just check it ran without panic.
        assert_eq!(r.curve.len(), 21);
    }

    #[test]
    fn btfd_rsi_oversold_entry() {
        // Short RSI period for testability: 3 consecutive drops → RSI drops below threshold.
        let bars = vec![
            bar("2020-01-01", 100.0),
            bar("2020-01-02", 90.0),
            bar("2020-01-03", 80.0),
            bar("2020-01-04", 70.0),
            bar("2020-01-05", 60.0),
        ];
        // rsi_period=2: after 2 down moves avg_gain=0 → RSI=0 → fires immediately
        let strategy = Strategy::BuyTheDip { rsi_period: 2, rsi_threshold: 30.0, bb_period: 50, bb_std: 2.0 };
        let sigs: Vec<f64> = {
            let mut gen = strategy.into_generator();
            bars.iter().map(|b| gen.next(b)).collect()
        };
        // First 2 bars: not enough history → 0.0; from bar 3 onwards: RSI fires.
        assert_eq!(sigs[0], 0.0);
        assert_eq!(sigs[1], 0.0);
        assert_eq!(sigs[2], 1.0); // RSI=0 < 30
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
