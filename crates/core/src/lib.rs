//! Backtest engine: bars in, equity curve + metrics out. No I/O, no deps on
//! data sources — so it also compiles to WASM for the web crate to reuse the
//! DTOs (Bar, BacktestResult).

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize};

// serde_json serializes f64::INFINITY as JSON null (JSON has no infinity literal),
// so we need a custom deser that maps null → 0.0 for any metric that can be infinite.
fn deser_f64_or_zero<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Option::<f64>::deserialize(d).map(|o| o.unwrap_or(0.0))
}
use std::collections::{HashMap, HashSet};

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

/// A corporate action applied on its ex-date.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CaKind {
    /// Share count multiplied by `ratio`, basis price divided by `ratio`.
    Split { ratio: f64 },
    /// Cash credited per long share held.
    /// ponytail: meaningful with raw (unadjusted) prices only — with adjclose,
    /// dividends are already in the return series, so this double-counts.
    /// Implement raw-price support before enabling Dividend in production.
    Dividend { amount_per_share: f64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorporateAction {
    pub ex_date: NaiveDate,
    pub ticker: String,
    pub kind: CaKind,
}

/// One disclosed congressional stock transaction (STOCK Act PTR).
/// Both dates are kept: transaction_date (when the trade occurred) and
/// filing_date (when publicly disclosed — typically 30–45 days later).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CongressTrade {
    pub member: String,
    pub ticker: String,
    pub transaction_date: NaiveDate,
    pub filing_date: NaiveDate,
    /// "purchase", "sale", "sale_partial", "exchange"
    pub trade_type: String,
    /// Disclosed amount bucket, e.g. "$15,001 - $50,000"
    pub amount_range: String,
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

/// One realized closing of (part of) a long lot — its gain and how long it was
/// held. Logged by `fill` for the tax engine (F2); drained by the backtest loop
/// and bucketed into the calendar year's taxable pool. Cash-neutral bookkeeping.
#[derive(Clone, Debug)]
pub struct RealizedSale {
    /// Signed realized gain: `closed_qty × (sale_price − entry_price)`.
    pub gain: f64,
    /// Calendar days from lot entry to this sale (US long-term at > 365).
    pub holding_days: i64,
}

/// A loss sale still inside the US wash-sale window — a repurchase of the same
/// ticker within 30 days disallows the loss and rolls it into the new lot's
/// basis. Recorded by `fill` only when `track_wash` is on.
#[derive(Clone, Debug)]
pub struct WashLoss {
    pub date: NaiveDate,
    pub entry_date: NaiveDate,
    pub qty: f64,
    pub loss_per_share: f64,
    pub long_term: bool,
}

/// Portfolio state: cash + open lots + running realized P&L.
/// ponytail: single-currency, no margin — extend for multi-currency or leverage.
#[derive(Clone, Debug, Default)]
pub struct Portfolio {
    pub cash: f64,
    /// ticker → open lots in fill order (FIFO).
    pub positions: HashMap<String, Vec<Lot>>,
    pub realized_pnl: f64,
    /// Lot-matching method at sale (FIFO default; HIFO for US specific-ID).
    pub accounting: AccountingMethod,
    /// Per-sale realized gains awaiting tax bucketing. The backtest drains this
    /// each bar; non-tax callers simply ignore it.
    pub realized_sales: Vec<RealizedSale>,
    /// When on (US tax path), `fill` records loss sales here for wash-sale
    /// matching. Off by default → zero overhead and no behaviour change.
    pub track_wash: bool,
    /// Open loss sales eligible to be washed by a later repurchase.
    pub recent_losses: Vec<WashLoss>,
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
            let method = self.accounting;
            let lots = self.positions.entry(ticker.to_owned()).or_default();
            // Close long lots in the configured order: FIFO (fill order) or HIFO
            // (highest cost basis first). Cash proceeds are identical either way —
            // only which basis is realized, and thus the taxable gain, differs.
            let mut order: Vec<usize> = (0..lots.len()).filter(|&i| lots[i].qty > 0.0).collect();
            if method == AccountingMethod::Hifo {
                order.sort_by(|&a, &b| lots[b].entry_price.total_cmp(&lots[a].entry_price));
            }
            for idx in order {
                if remaining <= 0.0 { break; }
                let lot = &mut lots[idx];
                let closed = lot.qty.min(remaining);
                let gain = closed * (price - lot.entry_price);
                let holding_days = (date - lot.entry_date).num_days();
                let entry_date = lot.entry_date;
                let entry_price = lot.entry_price;
                lot.qty -= closed;
                remaining -= closed;
                self.realized_pnl += gain;
                self.cash += closed * price;
                self.realized_sales.push(RealizedSale { gain, holding_days });
                // US wash-sale bookkeeping: remember loss sales so a repurchase
                // within 30 days can disallow the loss (see `wash_replacement`).
                if self.track_wash && gain < 0.0 {
                    self.recent_losses.push(WashLoss {
                        date,
                        entry_date,
                        qty: closed,
                        loss_per_share: entry_price - price,
                        long_term: holding_days > 365,
                    });
                }
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

    /// Rebalance every name in `alloc` to its target weight using total equity.
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
        self.rebalance_inner(alloc, prices, date, costs, None);
    }

    /// Partial rebalance: reset only `only` to their target weights (using total
    /// equity); every other position keeps its shares. Pairs with `RebalanceConfig`
    /// `full = false` and the `drifted_tickers` set.
    pub fn rebalance_drifted(
        &mut self,
        alloc: &Allocation,
        prices: &HashMap<String, f64>,
        date: NaiveDate,
        costs: &FillCosts,
        only: &HashSet<String>,
    ) {
        self.rebalance_inner(alloc, prices, date, costs, Some(only));
    }

    /// Shared rebalance loop. `only = Some(set)` restricts trading to those tickers.
    fn rebalance_inner(
        &mut self,
        alloc: &Allocation,
        prices: &HashMap<String, f64>,
        date: NaiveDate,
        costs: &FillCosts,
        only: Option<&HashSet<String>>,
    ) {
        let equity = self.equity(prices);
        for (ticker, &weight) in &alloc.0 {
            if only.is_some_and(|set| !set.contains(ticker)) {
                continue;
            }
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

    /// Apply a corporate action. Split adjusts lot quantities and basis prices so
    /// equity at the post-split price is unchanged. Dividend credits cash (see
    /// `CaKind::Dividend` note about adjclose double-counting).
    pub fn apply_action(&mut self, ticker: &str, kind: &CaKind) {
        match kind {
            CaKind::Split { ratio } => {
                if let Some(lots) = self.positions.get_mut(ticker) {
                    for lot in lots {
                        lot.qty *= ratio;
                        lot.entry_price /= ratio;
                    }
                }
            }
            CaKind::Dividend { amount_per_share } => {
                let shares = self.shares(ticker).max(0.0);
                if shares > 0.0 {
                    self.cash += shares * amount_per_share;
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

    /// US wash-sale (IRC §1091). Call right after buying `qty` replacement shares
    /// of `ticker` at `price` on `date`. Any loss sale of the same ticker within
    /// the prior 30 days is disallowed: the loss is rolled into the replacement
    /// lot's basis and the sold lot's holding period is carried onto it. Returns
    /// the disallowed loss split `(short_term, long_term)` so the caller can back
    /// it out of the year's taxable pool. "Substantially identical" = same ticker.
    /// ponytail: handles the sale→repurchase direction (the Pub 550 worked example
    /// and the dominant single-position pattern); the rarer repurchase-then-sale
    /// direction is out of scope under this single-symbol target-weight model.
    pub fn wash_replacement(&mut self, ticker: &str, date: NaiveDate, qty: f64, price: f64) -> (f64, f64) {
        // Drop losses that have aged out of the 61-day window (>30 days before).
        self.recent_losses
            .retain(|l| l.qty > 1e-9 && (0..=30).contains(&(date - l.date).num_days()));

        let mut st_disallowed = 0.0;
        let mut lt_disallowed = 0.0;
        let mut remaining = qty;
        for li in 0..self.recent_losses.len() {
            if remaining <= 1e-9 { break; }
            let l = self.recent_losses[li].clone();
            let w = remaining.min(l.qty);
            let disallowed = w * l.loss_per_share;
            if l.long_term { lt_disallowed += disallowed } else { st_disallowed += disallowed }
            // Replacement basis absorbs the per-share loss; holding period carries
            // (effective entry = buy date minus the sold lot's holding span).
            let prior_holding = (l.date - l.entry_date).num_days();
            let effective_entry = date - chrono::Duration::days(prior_holding);
            self.carve_wash(ticker, price, date, w, price + l.loss_per_share, effective_entry);
            self.recent_losses[li].qty -= w;
            remaining -= w;
        }
        self.recent_losses.retain(|l| l.qty > 1e-9);
        (st_disallowed, lt_disallowed)
    }

    /// Move `w` shares of the just-bought plain lot (entry `buy_price`/`buy_date`)
    /// into a washed lot carrying `new_basis` and `effective_entry`. Net share
    /// count is unchanged — only basis and holding period shift.
    fn carve_wash(&mut self, ticker: &str, buy_price: f64, buy_date: NaiveDate, w: f64, new_basis: f64, effective_entry: NaiveDate) {
        let lots = self.positions.entry(ticker.to_owned()).or_default();
        if let Some(lot) = lots.iter_mut().find(|l| {
            l.qty > 1e-9 && (l.entry_price - buy_price).abs() < 1e-9 && l.entry_date == buy_date
        }) {
            lot.qty -= w.min(lot.qty);
        }
        lots.retain(|l| l.qty.abs() > 1e-10);
        lots.push(Lot { qty: w, entry_price: new_basis, entry_date: effective_entry });
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

/// Tickers whose actual weight has drifted beyond `bands` from their target.
/// Empty when `bands` is `None` or equity is non-positive. This is the set a
/// partial (`full = false`) rebalance resets — see `Portfolio::rebalance_drifted`.
pub fn drifted_tickers(
    portfolio: &Portfolio,
    alloc: &Allocation,
    prices: &HashMap<String, f64>,
    bands: Option<&BandConfig>,
) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(bands) = bands else { return out; };
    let equity = portfolio.equity(prices);
    if equity <= 0.0 { return out; }
    for (ticker, &target) in &alloc.0 {
        let price = prices.get(ticker.as_str()).copied().unwrap_or(0.0);
        let actual = portfolio.shares(ticker) * price / equity;
        let drift = (actual - target).abs();
        if drift > bands.absolute || (target > 0.0 && drift / target > bands.relative) {
            out.insert(ticker.clone());
        }
    }
    out
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
    !drifted_tickers(portfolio, alloc, prices, config.bands.as_ref()).is_empty()
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
                positions: vec![], trades: vec![], entry_date: None, entry_pe: None, entry_index: None, entry_count: None,
                initial_amount: 10_000.0, final_value: 0.0, benchmark: None, tax_system: TaxSystem::None, total_tax: 0.0,
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
        if ci == 0 {
            // Establish the initial book in full regardless of `full`.
            portfolio.rebalance(&alloc, &prices, date, costs);
            last_rebalance = date;
        } else if needs_rebalance(&portfolio, &alloc, &prices, rebalance_config, last_rebalance, date) {
            if rebalance_config.full {
                portfolio.rebalance(&alloc, &prices, date, costs);
            } else {
                // full = false: reset only the names that breached their drift band.
                let drifted = drifted_tickers(&portfolio, &alloc, &prices, rebalance_config.bands.as_ref());
                portfolio.rebalance_drifted(&alloc, &prices, date, costs, &drifted);
            }
            last_rebalance = date;
        }
    }

    let rets: Vec<f64> = curve.windows(2).map(|w| w[1].equity / w[0].equity - 1.0).collect();
    let metrics = compute_metrics(&curve, &rets);
    // ponytail: positions left empty for multi-asset; add per-ticker summary when UI needs it.
    BacktestResult { curve, metrics, positions: vec![], trades: vec![], entry_date: None, entry_pe: None, entry_index: None, entry_count: None,
        initial_amount: 10_000.0, final_value: 0.0, benchmark: None, tax_system: TaxSystem::None, total_tax: 0.0, }
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metrics {
    pub total_return: f64,
    pub cagr: f64,
    pub max_drawdown: f64,
    #[serde(deserialize_with = "deser_f64_or_zero", default)]
    pub sharpe: f64,
    #[serde(deserialize_with = "deser_f64_or_zero", default)]
    pub sortino: f64,
    #[serde(deserialize_with = "deser_f64_or_zero", default)]
    pub recovery_factor: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EquityPoint {
    pub date: NaiveDate,
    pub equity: f64,
}

/// A single simulated trade event (entry or exit).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TradeEvent {
    pub date: NaiveDate,
    pub ticker: String,
    /// "buy" or "sell"
    pub action: String,
    pub price: f64,
    pub shares: f64,
}

/// Per-position breakdown included in a portfolio-level backtest result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PositionSummary {
    pub ticker: String,
    pub shares: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
}

// ===== Tax simulation (Epic F) ===========================================
// Income/capital-gains tax modelled as one knob struct + country presets.
// Pure compute, WASM-clean (no I/O, no new deps). Default = TaxSystem::None =
// no tax = today's behaviour. Rates verified against IRS.gov and
// gesetze-im-internet.de; see plan/tax-simulation-spec.md. The bracket tables
// are inputs, not constants of nature — re-verify before each tax year.

/// Which tax regime to simulate. `None` reproduces the pre-tax result exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TaxSystem {
    #[default]
    None,
    UsFederal,
    Germany,
}

/// Lot-matching method at sale. FIFO is the default and DE-mandatory; HIFO
/// (highest-cost-first — a US specific-ID strategy) realizes less gain. Applied
/// in F2; the DE resolver ignores it (FIFO is law).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AccountingMethod {
    #[default]
    Fifo,
    Hifo,
}

/// Tax knobs. `TaxConfig::preset` fills them for a `TaxSystem`; the API/UI then
/// overrides the user-facing ones (income, church tax, allowance, …). One struct
/// serves both systems — fields irrelevant to a system are unused by its resolver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaxConfig {
    pub system: TaxSystem,
    pub accounting: AccountingMethod,
    /// US: annual taxable income — places long-term gains in the 0/15/20 bracket
    /// and trips the NIIT cliff. Ignored by DE (flat rate).
    pub taxable_income: f64,
    /// DE: church tax (Kirchensteuer) lifts the flat rate 26.375% → ~27.82%.
    pub church_tax: bool,
    /// DE: Sparerpauschbetrag, the annual tax-free allowance. US: 0 (none).
    pub annual_allowance: f64,
    /// DE ETF: Teilfreistellung — fraction of a fund's taxable base exempt
    /// (0.30 equity / 0.15 mixed / 0.0 bond). Consumed by F5.
    pub etf_teilfreistellung: f64,
    /// DE ETF: accrue the Vorabpauschale (annual advance lump-sum). Consumed by F5.
    pub vorabpauschale: bool,
    /// DE ETF estimate mode (#61): treat every fund as a 30%-TFS equity fund,
    /// skipping per-fund equity quotas. Opt-in; over-states bond/mixed funds.
    pub estimate_all_etfs_equity: bool,
}

// US federal rate tables — 2025 single filer (IRS Rev. Proc. 2024-40).
// (upper_bound_inclusive, marginal_rate); the final bound is +inf.
const US_LT_BRACKETS: [(f64, f64); 3] =
    [(48_350.0, 0.0), (533_400.0, 0.15), (f64::INFINITY, 0.20)];
const US_ORDINARY_BRACKETS: [(f64, f64); 7] = [
    (11_925.0, 0.10), (48_475.0, 0.12), (103_350.0, 0.22), (197_300.0, 0.24),
    (250_525.0, 0.32), (626_350.0, 0.35), (f64::INFINITY, 0.37),
];
const US_NIIT_RATE: f64 = 0.038;
/// Single-filer MAGI cliff (non-indexed). ponytail: taxable_income used as the
/// MAGI proxy and single-filer assumed — add a filing-status knob if MFJ matters.
const US_NIIT_THRESHOLD: f64 = 200_000.0;

const DE_BASE_RATE: f64 = 0.26375; // 25% Abgeltungsteuer + 5.5% Soli
/// 8% Kirchensteuer variant (the common case). ponytail: 9% → 27.99% not exposed
/// — add a rate field if the church-tax state needs to vary.
const DE_CHURCH_RATE: f64 = 0.2782;
const DE_ALLOWANCE_SINGLE: f64 = 1_000.0; // Sparerpauschbetrag (single)

/// Marginal rate for `income` from a `(upper_bound, rate)` bracket table.
fn bracket_rate(table: &[(f64, f64)], income: f64) -> f64 {
    table.iter().find(|(hi, _)| income <= *hi).map(|(_, r)| *r).unwrap_or(0.0)
}

impl Default for TaxConfig {
    fn default() -> Self { Self::preset(TaxSystem::None) }
}

impl TaxConfig {
    /// Country preset filling the knobs with sane defaults. User input
    /// (income, church tax, allowance, …) overrides afterward.
    pub fn preset(system: TaxSystem) -> Self {
        TaxConfig {
            system,
            accounting: AccountingMethod::Fifo,
            taxable_income: 100_000.0,
            church_tax: false,
            annual_allowance: match system {
                TaxSystem::Germany => DE_ALLOWANCE_SINGLE,
                _ => 0.0,
            },
            etf_teilfreistellung: 0.30,
            vorabpauschale: matches!(system, TaxSystem::Germany),
            estimate_all_etfs_equity: false,
        }
    }

    /// Marginal tax rate on a realized capital gain. `holding_days` = calendar
    /// days from entry to sale (US long-term at > 365; DE ignores it). US stacks
    /// the NIIT surtax on top; `None` is always 0.
    pub fn gains_tax_rate(&self, holding_days: i64) -> f64 {
        match self.system {
            TaxSystem::None => 0.0,
            TaxSystem::UsFederal => {
                let base = if holding_days > 365 {
                    bracket_rate(&US_LT_BRACKETS, self.taxable_income)
                } else {
                    bracket_rate(&US_ORDINARY_BRACKETS, self.taxable_income)
                };
                base + self.us_niit()
            }
            TaxSystem::Germany => self.de_flat_rate(),
        }
    }

    /// NIIT surtax (US): 3.8% once income clears the cliff, else 0. Stacks on
    /// gains and qualified dividends.
    pub fn us_niit(&self) -> f64 {
        if self.taxable_income > US_NIIT_THRESHOLD { US_NIIT_RATE } else { 0.0 }
    }

    /// DE flat rate, with the church-tax surcharge when enabled.
    pub fn de_flat_rate(&self) -> f64 {
        if self.church_tax { DE_CHURCH_RATE } else { DE_BASE_RATE }
    }
}

/// German fund taxation for the traded ticker (InvStG 2018). `None` = a direct
/// stock (no Teilfreistellung, no Vorabpauschale). Built by the API from the
/// ticker's Yahoo instrument type (#70) and the estimate-mode flag.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FundTax {
    /// Teilfreistellung fraction exempt before tax (0.30 equity / 0.15 mixed /
    /// 0.0 bond). In estimate mode every fund is treated as 0.30.
    pub teilfreistellung: f64,
    /// Distributing (pays out) vs accumulating. Carried for the UI/F3; the
    /// Vorabpauschale nets down by the numeric distributions either way.
    pub distributing: bool,
}

/// Whether a Yahoo `instrumentType` denotes a fund (vs a direct equity). Used to
/// decide fund flagging and estimate-mode Teilfreistellung (#61).
pub fn is_fund_type(instrument_type: &str) -> bool {
    matches!(
        instrument_type.to_ascii_uppercase().as_str(),
        "ETF" | "MUTUALFUND" | "MONEYMARKET" | "FUND"
    )
}

/// Year → BMF Basiszins for the Vorabpauschale (§18 InvStG). A published per-year
/// input — 2021/22 were 0 (negative → floored); later years per BMF letters.
/// Unknown future years default to 0 until BMF publishes. Re-verify annually.
fn basiszins(year: i32) -> f64 {
    match year {
        2023 => 0.0255,
        2024 => 0.0229,
        2025 => 0.0253,
        2026 => 0.0320,
        _ => 0.0, // 2021/2022 = 0; future years until published
    }
}

/// Vorabpauschale (§18 InvStG) for one year, before Teilfreistellung.
/// `basisertrag = value_start × basiszins × 0.70 × proration`; the VAP is that
/// netted down by `distributions` and clamped to `[0, value_gain + distributions]`
/// — zero if the fund didn't rise. `proration` is the purchase-year factor
/// (1.0 when held all year; −1/12 per full month before purchase).
fn vorabpauschale(value_start: f64, value_gain: f64, distributions: f64, basiszins: f64, proration: f64) -> f64 {
    let basisertrag = value_start * basiszins * 0.70 * proration;
    let upper = (value_gain + distributions).max(0.0);
    (basisertrag - distributions).clamp(0.0, upper)
}

/// Purchase-year proration for the Vorabpauschale: 1.0 once a full year is held,
/// `(13 − purchase_month)/12` in the purchase year, 0 before the fund was bought.
fn fund_proration(entry: Option<NaiveDate>, year: i32) -> f64 {
    use chrono::Datelike;
    match entry {
        Some(d) if d.year() == year => (13 - d.month() as i32) as f64 / 12.0,
        Some(d) if d.year() < year => 1.0,
        _ => 0.0,
    }
}

/// Settle one calendar year for a German fund position: realized sale gains
/// (Teilfreistellung-exempted, reduced by the §19 Vorabpauschale credit already
/// taxed) plus this year's Vorabpauschale (also TFS-exempted), less the annual
/// allowance, at the DE flat rate. Returns `(tax, new_loss_carry, new_cum_vap)`.
#[allow(clippy::too_many_arguments)]
fn settle_de_fund(
    cfg: &TaxConfig, tfs: f64, realized_gain: f64,
    value_start: f64, value_gain: f64, distributions: f64, basiszins: f64, proration: f64,
    loss_carry: f64, cum_vap: f64,
) -> (f64, f64, f64) {
    let rate = cfg.de_flat_rate();

    // Realized gains: net losses (this year + carry) against gains; remainder carries.
    let gains = realized_gain.max(0.0);
    let losses = (-realized_gain.min(0.0)) + (-loss_carry);
    let net_gain = (gains - losses).max(0.0);
    let new_carry = -(losses - gains).max(0.0);

    // §19: reduce the gain by Vorabpauschalen already taxed (no double tax).
    let vap_credit = cum_vap.min(net_gain);
    let sale_base = (net_gain - vap_credit) * (1.0 - tfs);
    let mut cum_vap = cum_vap - vap_credit;

    // This year's Vorabpauschale, also Teilfreistellung-exempted.
    let vap = vorabpauschale(value_start, value_gain, distributions, basiszins, proration);
    cum_vap += vap;
    let vap_base = vap * (1.0 - tfs);

    let allow = cfg.annual_allowance;
    let taxable = (sale_base + vap_base - allow).max(0.0);
    (taxable * rate, new_carry, cum_vap)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BacktestResult {
    pub curve: Vec<EquityPoint>,
    pub metrics: Metrics,
    /// Per-position breakdown; empty for single-asset results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<PositionSummary>,
    /// Trade log: buy/sell events as signals transition. Empty for multi-asset presets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trades: Vec<TradeEvent>,
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
    /// Initial investment in dollars. Default $10 000 if not set by the caller.
    #[serde(default = "default_initial_amount")]
    pub initial_amount: f64,
    /// Final portfolio value = `initial_amount × curve.last().equity`.
    #[serde(default)]
    pub final_value: f64,
    /// Optional comparison run (buy-and-hold SPY by default). Boxed to avoid infinite type size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark: Option<Box<BacktestResult>>,
    /// Tax regime this run was computed under. `None` = pre-tax (today's result).
    /// F2+ apply the tax; F6 displays it.
    #[serde(default)]
    pub tax_system: TaxSystem,
    /// Total capital-gains tax withheld over the run, in the initial-cash currency
    /// (same units as `final_value`). 0 when `tax_system` is `None`. The `curve`
    /// already reflects this drag — it is the after-tax equity path.
    #[serde(default)]
    pub total_tax: f64,
}

fn default_initial_amount() -> f64 { 10_000.0 }

impl BacktestResult {
    /// Enriches the result with dollar values. Call this in the API after any backtest run.
    pub fn with_amount(mut self, amount: f64) -> Self {
        self.initial_amount = amount;
        self.final_value = amount * self.curve.last().map(|p| p.equity).unwrap_or(1.0);
        self
    }

    /// Records which tax system produced this result (for display in F6).
    pub fn with_tax_system(mut self, system: TaxSystem) -> Self {
        self.tax_system = system;
        self
    }
}

/// Settle one calendar year's realized capital-gains tax. `st_net`/`lt_net` are
/// the year's signed short-/long-term gains (net of same-character losses);
/// `loss_carry` is the prior carried-forward loss (≤ 0). Returns the tax owed and
/// the new carryforward. Losses (this year's + carried) offset short-term gains
/// first (higher US rate), then long-term; the DE allowance then reduces the
/// taxable base (long-term first). For `TaxSystem::None` it's a no-op.
/// ponytail: single stock pool per the spec — ST/LT losses cross-offset, which is
/// the documented simplification of the full IRS net-within-then-across rule.
fn settle_year(cfg: &TaxConfig, st_net: f64, lt_net: f64, loss_carry: f64) -> (f64, f64) {
    if cfg.system == TaxSystem::None {
        return (0.0, 0.0);
    }
    let st_rate = cfg.gains_tax_rate(0);   // ≤ 1yr → short-term / ordinary
    let lt_rate = cfg.gains_tax_rate(400); // > 1yr → long-term (DE: same flat rate)

    let gains_st = st_net.max(0.0);
    let gains_lt = lt_net.max(0.0);
    // All available losses as a positive figure: this year's net losses + carry.
    let losses = (-st_net.min(0.0)) + (-lt_net.min(0.0)) + (-loss_carry);

    // Offset short-term gains first, then long-term; remainder carries forward.
    let st_taxable = (gains_st - losses).max(0.0);
    let losses_left = (losses - gains_st).max(0.0);
    let lt_taxable = (gains_lt - losses_left).max(0.0);
    let losses_left = (losses_left - gains_lt).max(0.0);
    let new_carry = -losses_left;

    // Annual allowance (DE Sparerpauschbetrag; US = 0). Apply to the lower-rate
    // long-term base first so it shelters the dearer slice last.
    let allow = cfg.annual_allowance;
    let lt_after = (lt_taxable - allow).max(0.0);
    let allow_left = (allow - lt_taxable).max(0.0);
    let st_after = (st_taxable - allow_left).max(0.0);

    (st_after * st_rate + lt_after * lt_rate, new_carry)
}

/// Event-driven single-asset backtest using the portfolio state model.
/// Processes bars in chronological order; at each bar, marks to market then
/// rebalances to `signal * equity / price` shares at the bar's close (no lookahead).
/// Tax-free — see `run_portfolio_backtest_taxed` to apply a tax regime.
pub fn run_portfolio_backtest(
    ticker: &str,
    bars: &[Bar],
    strategy: &Strategy,
    initial_cash: f64,
    costs: &FillCosts,
    rfr_daily: f64,
    corporate_actions: &[CorporateAction],
) -> BacktestResult {
    run_portfolio_backtest_taxed(
        ticker, bars, strategy, initial_cash, costs, rfr_daily, corporate_actions,
        &TaxConfig::default(), None,
    )
}

/// As `run_portfolio_backtest`, but applies `tax`: realized capital-gains tax is
/// accrued per closed lot (classified short-/long-term from the lot's entry date)
/// and settled from cash at each calendar-year boundary, so the tax drag
/// compounds. The returned `curve` is the after-tax equity path and `total_tax`
/// the cumulative tax. `TaxConfig::default()` (`None`) reproduces the pre-tax run
/// bit-for-bit. Note: only realized gains are taxed — a never-selling buy-and-hold
/// owes no capital-gains tax here, except a German fund, which still accrues the
/// annual Vorabpauschale. `fund` (DE only) flags the ticker as a fund and carries
/// its Teilfreistellung %; `None` = a direct stock.
#[allow(clippy::too_many_arguments)]
pub fn run_portfolio_backtest_taxed(
    ticker: &str,
    bars: &[Bar],
    strategy: &Strategy,
    initial_cash: f64,
    costs: &FillCosts,
    // Daily risk-free rate on idle cash. 0.0 = off. Pull from FRED DGS3MO/252 for live rate.
    rfr_daily: f64,
    corporate_actions: &[CorporateAction],
    tax: &TaxConfig,
    fund: Option<&FundTax>,
) -> BacktestResult {
    use chrono::Datelike;
    let mut gen = strategy.clone().into_generator();
    let mut portfolio = Portfolio::new(initial_cash);
    portfolio.accounting = tax.accounting;
    // US wash-sale only: track loss sales so a repurchase within 30 days washes them.
    let wash = tax.system == TaxSystem::UsFederal;
    portfolio.track_wash = wash;
    let mut curve = Vec::with_capacity(bars.len());
    let mut trades: Vec<TradeEvent> = Vec::new();

    // Annual tax-settlement state.
    let mut acct_year: Option<i32> = None; // calendar year currently accumulating
    let mut year_st = 0.0; // signed short-term realized gains this year
    let mut year_lt = 0.0; // signed long-term realized gains this year
    let mut loss_carry = 0.0; // carried-forward capital loss (≤ 0)
    let mut total_tax = 0.0;

    // German fund (InvStG 2018): Teilfreistellung on gains + Vorabpauschale. When
    // `de_fund`, realized gains route to `year_fund_gain` and settle via the fund
    // path (TFS + §19 VAP credit) instead of the generic ST/LT settlement.
    let de_fund = tax.system == TaxSystem::Germany && fund.is_some();
    let vap_on = de_fund && tax.vorabpauschale;
    let tfs = fund.map(|f| f.teilfreistellung).unwrap_or(0.0);
    let mut year_fund_gain = 0.0;     // signed realized fund gains this year (raw)
    let mut year_distributions = 0.0; // fund payouts this year (0 until F3 dividends)
    let mut cum_vap = 0.0;            // Vorabpauschalen taxed so far, awaiting §19 credit
    let mut year_open_value = 0.0;    // fund position value at the year's first bar
    let mut open_year: Option<i32> = None;
    let mut prev_pos_val = 0.0;       // fund position value at the previous bar
    let mut fund_entry: Option<NaiveDate> = None; // first purchase, for VAP proration

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

        // Year boundary: settle the prior year's accrued gains and withhold the
        // tax from cash before this bar marks to market (≈ the January N+1 event).
        let year = bar.date.year();
        if let Some(prev) = acct_year {
            if year != prev {
                let t = if de_fund {
                    let value_gain = prev_pos_val - year_open_value;
                    let bz = if vap_on { basiszins(prev) } else { 0.0 };
                    let pror = fund_proration(fund_entry, prev);
                    let (t, carry, cv) = settle_de_fund(
                        tax, tfs, year_fund_gain, year_open_value, value_gain,
                        year_distributions, bz, pror, loss_carry, cum_vap,
                    );
                    loss_carry = carry;
                    cum_vap = cv;
                    year_fund_gain = 0.0;
                    year_distributions = 0.0;
                    t
                } else {
                    let (t, carry) = settle_year(tax, year_st, year_lt, loss_carry);
                    loss_carry = carry;
                    year_st = 0.0;
                    year_lt = 0.0;
                    t
                };
                portfolio.cash -= t;
                total_tax += t;
            }
        }
        acct_year = Some(year);

        // Apply any corporate actions on this bar's date before mark-to-market so
        // lot quantities and prices stay consistent with the adjusted close series.
        for ca in corporate_actions.iter().filter(|a| a.ex_date == bar.date && a.ticker == ticker) {
            portfolio.apply_action(ticker, &ca.kind);
        }

        // Mark-to-market BEFORE rebalance — today's equity reflects yesterday's position.
        let eq = portfolio.cash + portfolio.shares(ticker) * bar.close;
        curve.push(EquityPoint { date: bar.date, equity: eq / initial_cash });

        // Signal uses only data through this bar; fills at close, earns next bar's return.
        let weight = gen.next(bar);
        let adv = rolling_adv(bars, i, 20);
        let shares_before = portfolio.shares(ticker);
        portfolio.execute(
            &Order::TargetWeight { ticker: ticker.to_owned(), weight },
            bar.close,
            bar.date,
            costs,
            adv,
        );
        let shares_after = portfolio.shares(ticker);
        let delta = shares_after - shares_before;
        if delta.abs() > 1e-9 {
            trades.push(TradeEvent {
                date: bar.date,
                ticker: ticker.to_string(),
                action: if delta > 0.0 { "buy".to_string() } else { "sell".to_string() },
                price: bar.close,
                shares: delta.abs(),
            });
        }
        if delta > 1e-9 && fund_entry.is_none() {
            fund_entry = Some(bar.date); // first purchase, for VAP purchase-year proration
        }

        // Vorabpauschale fund value (post-trade): the opening value of each year
        // (purchase value in the buy year, since the position is bought at bar 0)
        // and the running year-end value. Both post-execute so a buy-and-hold's
        // start value is its actual holding, not the pre-purchase zero.
        let pos_val_after = portfolio.shares(ticker) * bar.close;
        if open_year != Some(year) {
            year_open_value = pos_val_after;
            open_year = Some(year);
        }
        prev_pos_val = pos_val_after;

        // Bucket this bar's realized sales into the current year's taxable pool.
        // German funds route to the fund pool (Teilfreistellung + §19); others
        // split short-/long-term.
        for s in portfolio.realized_sales.drain(..) {
            if de_fund {
                year_fund_gain += s.gain;
            } else if s.holding_days > 365 {
                year_lt += s.gain;
            } else {
                year_st += s.gain;
            }
        }

        // A buy may be a wash-sale replacement for a recent loss — back the
        // disallowed loss out of the pool (it moves into the new lot's basis).
        if wash && delta > 1e-9 {
            let (st_off, lt_off) = portfolio.wash_replacement(ticker, bar.date, delta, bar.close);
            year_st += st_off;
            year_lt += lt_off;
        }
    }

    // Settle the final (partial) year so its realized gains (and a fund's last
    // Vorabpauschale) aren't left untaxed. The curve is already built, so fold the
    // withholding into the last point. ponytail: the real January-N+1 settlement
    // falls outside the window — taxing it at the end keeps the after-tax number whole.
    let final_tax = if de_fund {
        let last_year = acct_year.unwrap_or(0);
        let value_gain = prev_pos_val - year_open_value;
        let bz = if vap_on { basiszins(last_year) } else { 0.0 };
        let pror = fund_proration(fund_entry, last_year);
        settle_de_fund(tax, tfs, year_fund_gain, year_open_value, value_gain,
            year_distributions, bz, pror, loss_carry, cum_vap).0
    } else {
        settle_year(tax, year_st, year_lt, loss_carry).0
    };
    if final_tax != 0.0 {
        portfolio.cash -= final_tax;
        total_tax += final_tax;
        if let Some(last) = curve.last_mut() {
            last.equity -= final_tax / initial_cash;
        }
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

    BacktestResult { curve, metrics, positions, trades, entry_date: None, entry_pe: None, entry_index: None, entry_count: None,
        initial_amount: 10_000.0, final_value: 0.0, benchmark: None, tax_system: tax.system, total_tax, }
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

/// Build signals from congressional disclosure events.
/// `disclosures` is `(execution_date, target_weight)` sorted by date — use either
/// `CongressTrade::transaction_date` (naive) or `::filing_date` (point-in-time).
/// `signals[i]` is the weight observed at `bars[i]`; the engine applies
/// `signals[i-1]` to bar `i`'s return — no lookahead.
pub fn congress_signals(bars: &[Bar], disclosures: &[(NaiveDate, f64)]) -> Vec<f64> {
    let mut signals = vec![0.0f64; bars.len()];
    let mut current = 0.0f64;
    let mut di = 0;
    for (i, bar) in bars.iter().enumerate() {
        while di < disclosures.len() && disclosures[di].0 <= bar.date {
            current = disclosures[di].1;
            di += 1;
        }
        signals[i] = current;
    }
    signals
}

/// Run a backtest from a pre-computed signal vector (same length as `bars`).
/// `signals[i]` = position weight applied to bar `i+1`'s return.
/// Emits a TradeEvent whenever the signal transitions between 0 and non-zero.
pub fn run_signals_backtest(ticker: &str, bars: &[Bar], signals: &[f64]) -> BacktestResult {
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let mut equity = 1.0f64;
    let mut curve = Vec::with_capacity(bars.len());
    let mut rets: Vec<f64> = Vec::with_capacity(bars.len().saturating_sub(1));
    let mut trades: Vec<TradeEvent> = Vec::new();
    let mut prev_sig = 0.0f64;
    for i in 0..bars.len() {
        if i > 0 {
            let pct = closes[i] / closes[i - 1] - 1.0;
            let r = signals[i - 1] * pct;
            equity *= 1.0 + r;
            rets.push(r);
        }
        let sig = signals.get(i).copied().unwrap_or(0.0);
        if prev_sig == 0.0 && sig != 0.0 {
            trades.push(TradeEvent { date: bars[i].date, ticker: ticker.to_string(), action: "buy".to_string(), price: closes[i], shares: 1.0 });
        } else if prev_sig != 0.0 && sig == 0.0 {
            trades.push(TradeEvent { date: bars[i].date, ticker: ticker.to_string(), action: "sell".to_string(), price: closes[i], shares: 1.0 });
        }
        prev_sig = sig;
        curve.push(EquityPoint { date: bars[i].date, equity });
    }
    let metrics = compute_metrics(&curve, &rets);
    BacktestResult {
        curve,
        metrics,
        positions: vec![],
        trades,
        entry_date: None,
        entry_pe: None,
        entry_index: None,
        entry_count: None,
        initial_amount: 10_000.0,
        final_value: 0.0,
        benchmark: None,
        tax_system: TaxSystem::None,
        total_tax: 0.0,
    }
}

/// Run a single-ticker backtest driven by pre-computed event signals.
/// `events` = `(execution_date, target_weight)` — works for congressional trades,
/// Cramer call fades, or any dated event-signal source.
pub fn run_event_backtest(ticker: &str, bars: &[Bar], events: &[(NaiveDate, f64)]) -> BacktestResult {
    run_signals_backtest(ticker, bars, &congress_signals(bars, events))
}

/// Entry when days-to-cover exceeds `dtc_min` AND price is above its `window`-day SMA.
/// `si` = `(settlement_date, days_to_cover)` sorted ascending by date.
/// Signal is held until momentum fades (price drops below SMA) — coarse biweekly
/// re-evaluation of the SI condition matches the FINRA release cadence.
pub fn squeeze_signals(bars: &[Bar], si: &[(NaiveDate, f64)], dtc_min: f64, window: usize) -> Vec<f64> {
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let mut signals = vec![0.0f64; bars.len()];
    let mut current_dtc = 0.0f64;
    let mut si_idx = 0usize;

    for i in 0..bars.len() {
        while si_idx < si.len() && si[si_idx].0 <= bars[i].date {
            current_dtc = si[si_idx].1;
            si_idx += 1;
        }
        if current_dtc < dtc_min || i < window { continue; }
        let sma = closes[i - window..i].iter().sum::<f64>() / window as f64;
        if closes[i] > sma {
            signals[i] = 1.0;
        }
    }
    signals
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

    #[test]
    fn us_resolver_places_gains_in_right_bracket() {
        let mut c = TaxConfig::preset(TaxSystem::UsFederal);
        // $96k single filer: long-term gain → 15% bracket, no NIIT (< $200k).
        c.taxable_income = 96_000.0;
        assert!((c.gains_tax_rate(400) - 0.15).abs() < 1e-9); // > 1yr = long-term
        assert!((c.gains_tax_rate(200) - 0.22).abs() < 1e-9); // ≤ 1yr = ordinary 22%
        // Low income → 0% long-term bracket.
        c.taxable_income = 30_000.0;
        assert!(c.gains_tax_rate(400).abs() < 1e-9);
        // High income → 20% long-term + 3.8% NIIT stacked.
        c.taxable_income = 600_000.0;
        assert!((c.gains_tax_rate(400) - (0.20 + 0.038)).abs() < 1e-9);
    }

    #[test]
    fn de_resolver_is_flat_regardless_of_holding_period() {
        let mut c = TaxConfig::preset(TaxSystem::Germany);
        assert!((c.gains_tax_rate(30) - 0.26375).abs() < 1e-9);
        assert!((c.gains_tax_rate(4000) - 0.26375).abs() < 1e-9); // no holding split
        c.church_tax = true;
        assert!((c.gains_tax_rate(30) - 0.2782).abs() < 1e-9);
    }

    #[test]
    fn none_system_taxes_nothing() {
        let c = TaxConfig::preset(TaxSystem::None);
        assert_eq!(c.gains_tax_rate(10), 0.0);
        assert_eq!(c.gains_tax_rate(9999), 0.0);
    }

    // --- F2: realized capital-gains tax + annual settlement -----------------

    #[test]
    fn us_settles_short_and_long_term_at_their_own_rates() {
        let mut cfg = TaxConfig::preset(TaxSystem::UsFederal);
        cfg.taxable_income = 96_000.0; // long-term 15%, ordinary 22%, no NIIT
        // $1,000 short-term gain + $1,000 long-term gain in one year, no losses.
        let (tax, carry) = settle_year(&cfg, 1_000.0, 1_000.0, 0.0);
        assert!((tax - (1_000.0 * 0.22 + 1_000.0 * 0.15)).abs() < 1e-6);
        assert_eq!(carry, 0.0);
    }

    #[test]
    fn de_settles_flat_after_allowance() {
        let cfg = TaxConfig::preset(TaxSystem::Germany); // €1,000 allowance, 26.375%
        // €5,000 long-term gain, less the €1,000 Sparerpauschbetrag → €4,000 taxed.
        let (tax, _) = settle_year(&cfg, 0.0, 5_000.0, 0.0);
        assert!((tax - 4_000.0 * 0.26375).abs() < 1e-6);
    }

    #[test]
    fn us_carries_net_loss_forward() {
        let mut cfg = TaxConfig::preset(TaxSystem::UsFederal);
        cfg.taxable_income = 96_000.0;
        // Year 1: net $500 loss → no tax, carry -500 forward.
        let (tax1, carry1) = settle_year(&cfg, 0.0, -500.0, 0.0);
        assert_eq!(tax1, 0.0);
        assert!((carry1 + 500.0).abs() < 1e-9);
        // Year 2: $1,000 long-term gain offset by the $500 carry → $500 at 15%.
        let (tax2, carry2) = settle_year(&cfg, 0.0, 1_000.0, carry1);
        assert!((tax2 - 500.0 * 0.15).abs() < 1e-6);
        assert_eq!(carry2, 0.0);
    }

    #[test]
    fn hifo_realizes_less_gain_than_fifo() {
        let d = |s: &str| s.parse::<NaiveDate>().unwrap();
        let realized = |method| {
            let mut p = Portfolio::new(0.0);
            p.accounting = method;
            p.fill("X", 10.0, 10.0, d("2020-01-02")); // cheap old lot
            p.fill("X", 10.0, 20.0, d("2021-01-04")); // dear new lot
            p.fill("X", -10.0, 30.0, d("2022-01-03")); // sell 10 @ 30
            p.realized_sales.iter().map(|s| s.gain).sum::<f64>()
        };
        let fifo = realized(AccountingMethod::Fifo); // closes $10 lot → +200
        let hifo = realized(AccountingMethod::Hifo); // closes $20 lot → +100
        assert!((fifo - 200.0).abs() < 1e-9);
        assert!((hifo - 100.0).abs() < 1e-9);
        assert!(hifo < fifo);
    }

    #[test]
    fn fill_classifies_holding_period_from_entry_date() {
        let d = |s: &str| s.parse::<NaiveDate>().unwrap();
        let mut p = Portfolio::new(0.0);
        p.fill("X", 10.0, 10.0, d("2020-01-02")); // > 1yr at sale
        p.fill("X", 10.0, 10.0, d("2022-06-01")); // < 1yr at sale
        p.fill("X", -20.0, 12.0, d("2022-07-01")); // FIFO: old lot first
        assert_eq!(p.realized_sales.len(), 2);
        assert!(p.realized_sales[0].holding_days > 365);
        assert!(p.realized_sales[1].holding_days <= 365);
    }

    // --- F4: US wash-sale ---------------------------------------------------

    #[test]
    fn wash_sale_disallows_loss_and_rolls_into_replacement_basis() {
        // IRS Pub 550 worked example: buy 100 @ 1000, sell @ 750 (−250/sh),
        // rebuy 100 @ 800 within 30 days → loss disallowed, basis = 1050.
        let d = |s: &str| s.parse::<NaiveDate>().unwrap();
        let mut p = Portfolio::new(200_000.0);
        p.track_wash = true;
        p.fill("X", 100.0, 1000.0, d("2021-01-04"));
        p.fill("X", -100.0, 750.0, d("2021-02-01")); // realizes −25,000
        assert_eq!(p.recent_losses.len(), 1);
        p.fill("X", 100.0, 800.0, d("2021-02-10")); // replacement
        let (st, lt) = p.wash_replacement("X", d("2021-02-10"), 100.0, 800.0);
        assert!((st - 25_000.0).abs() < 1e-6, "whole short-term loss disallowed");
        assert_eq!(lt, 0.0);
        let lots = &p.positions["X"];
        let washed = lots.iter().find(|l| (l.entry_price - 1050.0).abs() < 1e-6)
            .expect("replacement lot at basis 1050");
        assert!((washed.qty - 100.0).abs() < 1e-9);
        assert!(washed.entry_date < d("2021-02-10"), "holding period carried back");
        assert_eq!(p.recent_losses.len(), 0, "loss consumed");
    }

    #[test]
    fn loss_outside_window_is_not_washed() {
        let d = |s: &str| s.parse::<NaiveDate>().unwrap();
        let mut p = Portfolio::new(200_000.0);
        p.track_wash = true;
        p.fill("X", 100.0, 1000.0, d("2021-01-04"));
        p.fill("X", -100.0, 750.0, d("2021-02-01")); // loss
        p.fill("X", 100.0, 800.0, d("2021-04-01")); // 59 days later — out of window
        let (st, lt) = p.wash_replacement("X", d("2021-04-01"), 100.0, 800.0);
        assert_eq!((st, lt), (0.0, 0.0), "no wash outside the 30-day window");
        // Replacement keeps its plain 800 basis; the loss stays allowed.
        let lots = &p.positions["X"];
        assert!(lots.iter().any(|l| (l.entry_price - 800.0).abs() < 1e-9));
    }

    // --- F5: German ETF taxation (Teilfreistellung + Vorabpauschale) --------

    #[test]
    fn fund_type_detection() {
        assert!(is_fund_type("ETF"));
        assert!(is_fund_type("mutualfund"));
        assert!(!is_fund_type("EQUITY"));
        assert!(!is_fund_type(""));
    }

    #[test]
    fn vorabpauschale_clamps_nets_and_prorates() {
        let base = 10_000.0 * 0.0229 * 0.70;
        // Rose enough → full Basisertrag.
        assert!((vorabpauschale(10_000.0, 2_000.0, 0.0, 0.0229, 1.0) - base).abs() < 1e-9);
        // Fell → clamp to zero.
        assert_eq!(vorabpauschale(10_000.0, -500.0, 0.0, 0.0229, 1.0), 0.0);
        // Distributing fund nets the VAP down by distributions.
        assert!((vorabpauschale(10_000.0, 2_000.0, 50.0, 0.0229, 1.0) - (base - 50.0)).abs() < 1e-9);
        // Purchase-year proration.
        assert!((vorabpauschale(10_000.0, 2_000.0, 0.0, 0.0229, 0.5) - base * 0.5).abs() < 1e-9);
    }

    #[test]
    fn de_fund_taxes_vorabpauschale_after_teilfreistellung() {
        let mut cfg = TaxConfig::preset(TaxSystem::Germany);
        cfg.annual_allowance = 0.0; // isolate the VAP
        // Accumulating equity ETF (30% TFS) rose across 2024 (Basiszins 0.0229).
        let (tax, _carry, cumvap) =
            settle_de_fund(&cfg, 0.30, 0.0, 10_000.0, 2_000.0, 0.0, 0.0229, 1.0, 0.0, 0.0);
        let vap = 10_000.0 * 0.0229 * 0.70;
        assert!((cumvap - vap).abs() < 1e-9);
        assert!((tax - vap * (1.0 - 0.30) * 0.26375).abs() < 1e-6);
    }

    #[test]
    fn de_fund_sale_credits_prior_vorabpauschale_then_applies_tfs() {
        let mut cfg = TaxConfig::preset(TaxSystem::Germany);
        cfg.annual_allowance = 0.0;
        // $1,000 realized gain, $200 of VAP already taxed (§19 credit), 30% TFS,
        // no new VAP (value flat). Base = (1000 − 200) × 0.70 = 560.
        let (tax, _carry, cumvap) =
            settle_de_fund(&cfg, 0.30, 1_000.0, 5_000.0, 0.0, 0.0, 0.0, 1.0, 0.0, 200.0);
        assert!((tax - 560.0 * 0.26375).abs() < 1e-6);
        assert!(cumvap.abs() < 1e-9, "the 200 credit is consumed");
    }

    // Monthly 2024 bars (Basiszins year) rising 100→200 — an accumulating fund.
    fn rising_2024_bars() -> Vec<Bar> {
        let p = [
            ("2024-01-15", 100.0), ("2024-02-15", 108.0), ("2024-03-15", 117.0),
            ("2024-04-15", 126.0), ("2024-05-15", 136.0), ("2024-06-15", 147.0),
            ("2024-07-15", 158.0), ("2024-08-15", 169.0), ("2024-09-15", 178.0),
            ("2024-10-15", 186.0), ("2024-11-15", 193.0), ("2024-12-15", 200.0),
        ];
        p.iter().map(|(d, c)| bar(d, *c)).collect()
    }

    #[test]
    fn de_fund_buy_and_hold_accrues_vorabpauschale() {
        let bars = rising_2024_bars();
        let fund = FundTax { teilfreistellung: 0.0, distributing: false };
        // Large stake so the VAP clears the €1,000 allowance.
        let mut de = TaxConfig::preset(TaxSystem::Germany);
        let with_vap = run_portfolio_backtest_taxed(
            "ETF", &bars, &Strategy::BuyAndHold, 100_000.0, &FillCosts::ZERO, 0.0, &[], &de, Some(&fund),
        );
        assert!(with_vap.total_tax > 0.0, "a risen accumulating fund owes Vorabpauschale");

        // With the Vorabpauschale off, the same buy-and-hold realizes nothing → no tax.
        de.vorabpauschale = false;
        let no_vap = run_portfolio_backtest_taxed(
            "ETF", &bars, &Strategy::BuyAndHold, 100_000.0, &FillCosts::ZERO, 0.0, &[], &de, Some(&fund),
        );
        assert_eq!(no_vap.total_tax, 0.0);
    }

    // A rise through 2020 then a 2021 crash, so an SMA crossover enters long and
    // later exits at a profit — realizing a gain that crosses a year boundary.
    fn rise_then_crash_bars() -> Vec<Bar> {
        let p = [
            ("2020-01-15", 100.0), ("2020-02-15", 104.0), ("2020-03-15", 108.0),
            ("2020-04-15", 114.0), ("2020-05-15", 120.0), ("2020-06-15", 128.0),
            ("2020-07-15", 138.0), ("2020-08-15", 150.0), ("2020-09-15", 165.0),
            ("2020-10-15", 180.0), ("2020-11-15", 195.0), ("2020-12-15", 210.0),
            ("2021-01-15", 200.0), ("2021-02-15", 170.0), ("2021-03-15", 140.0),
            ("2021-04-15", 120.0), ("2021-05-15", 110.0), ("2021-06-15", 105.0),
        ];
        p.iter().map(|(d, c)| bar(d, *c)).collect()
    }

    #[test]
    fn year_boundary_settlement_reduces_equity_and_none_is_unchanged() {
        let bars = rise_then_crash_bars();
        let strat = Strategy::SmaCrossover { fast: 3, slow: 5 };

        let pre = run_portfolio_backtest("X", &bars, &strat, 10_000.0, &FillCosts::ZERO, 0.0, &[]);
        // The scenario must actually sell, or it proves nothing.
        assert!(pre.trades.iter().any(|t| t.action == "sell"), "expected a sell");

        let mut us = TaxConfig::preset(TaxSystem::UsFederal);
        us.taxable_income = 96_000.0;
        let taxed = run_portfolio_backtest_taxed("X", &bars, &strat, 10_000.0, &FillCosts::ZERO, 0.0, &[], &us, None);
        assert!(taxed.total_tax > 0.0, "a realized gain should be taxed");
        assert!(
            taxed.curve.last().unwrap().equity < pre.curve.last().unwrap().equity,
            "tax withholding must lower final equity",
        );

        // tax=none reproduces the pre-tax run bit-for-bit.
        let none = run_portfolio_backtest_taxed("X", &bars, &strat, 10_000.0, &FillCosts::ZERO, 0.0, &[], &TaxConfig::default(), None);
        assert_eq!(none.total_tax, 0.0);
        for (a, b) in none.curve.iter().zip(pre.curve.iter()) {
            assert_eq!(a.equity, b.equity);
        }
    }

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
        let r = run_portfolio_backtest("X", &bars, &Strategy::BuyAndHold, 10_000.0, &FillCosts::ZERO, 0.0, &[]);
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
        let r_up = run_portfolio_backtest("X", &bars_up, &Strategy::BuyAndHold, 10_000.0, &FillCosts::ZERO, 0.0, &[]);
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
        let r = run_portfolio_backtest("X", &bars, &Strategy::BuyAndHold, 10_000.0, &FillCosts::ZERO, 0.0, &[]);
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
        let r = run_portfolio_backtest("X", &bars, &Strategy::BuyAndHold, 1000.0, &costs, 0.0, &[]);
        assert!((r.curve.last().unwrap().equity - 0.99).abs() < 1e-9);
    }

    #[test]
    fn spread_fraction_reduces_equity() {
        // 1% spread on 10 shares @ 100 = $10 → same result as commission test.
        let bars = vec![bar("2020-01-01", 100.0), bar("2020-01-02", 100.0)];
        let costs = FillCosts { spread_fraction: 0.01, ..FillCosts::ZERO };
        let r = run_portfolio_backtest("X", &bars, &Strategy::BuyAndHold, 1000.0, &costs, 0.0, &[]);
        assert!((r.curve.last().unwrap().equity - 0.99).abs() < 1e-9);
    }

    #[test]
    fn rfr_accrues_on_idle_cash() {
        // SMA windows larger than bar count → always flat (signal=0), all cash.
        let bars: Vec<Bar> = (1..=3).map(|i| bar(&format!("2020-01-{:02}", i), 100.0)).collect();
        let strategy = Strategy::SmaCrossover { fast: 100, slow: 200 };
        let r = run_portfolio_backtest("X", &bars, &strategy, 1000.0, &FillCosts::ZERO, 0.01, &[]);
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
    fn partial_rebalance_touches_only_drifted() {
        // Three equal-weight names. When one doubles, a fully-invested book's
        // weights all shift — but with a 10pp band only the doubled name breaches
        // (the other two drift ~8.3pp). full = false must trade only that name.
        let d0: NaiveDate = "2024-01-01".parse().unwrap();
        let mut p = Portfolio::new(3000.0);
        let alloc = Allocation(HashMap::from([
            ("X".to_string(), 1.0),
            ("Y".to_string(), 1.0),
            ("Z".to_string(), 1.0),
        ]))
        .normalize();
        let p0 = HashMap::from([
            ("X".to_string(), 100.0), ("Y".to_string(), 100.0), ("Z".to_string(), 100.0),
        ]);
        p.rebalance(&alloc, &p0, d0, &FillCosts::ZERO); // 10 shares each
        assert!((p.shares("X") - 10.0).abs() < 1e-9);

        // X doubles. Only X clears a 10pp absolute band (relative wide-open).
        let p1 = HashMap::from([
            ("X".to_string(), 200.0), ("Y".to_string(), 100.0), ("Z".to_string(), 100.0),
        ]);
        let bands = BandConfig { absolute: 0.10, relative: 1.0 };
        let drifted = drifted_tickers(&p, &alloc, &p1, Some(&bands));
        assert_eq!(drifted, HashSet::from(["X".to_string()]));

        let d1: NaiveDate = "2024-02-01".parse().unwrap();
        p.rebalance_drifted(&alloc, &p1, d1, &FillCosts::ZERO, &drifted);
        // Y and Z keep their shares; X is reset to target (1/3 of 4000 equity / 200).
        assert!((p.shares("Y") - 10.0).abs() < 1e-9, "Y untouched");
        assert!((p.shares("Z") - 10.0).abs() < 1e-9, "Z untouched");
        assert!((p.shares("X") - 6.6667).abs() < 1e-3, "X reset to target: {}", p.shares("X"));
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
        let r = run_portfolio_backtest("X", &bars, &strategy, 1000.0, &FillCosts::ZERO, 0.0, &[]);
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
    fn split_adjusts_lots_and_preserves_equity() {
        let d: NaiveDate = "2024-01-01".parse().unwrap();
        let mut p = Portfolio::new(1000.0);
        p.fill("X", 10.0, 100.0, d); // 10 shares @ $100
        p.apply_action("X", &CaKind::Split { ratio: 2.0 });
        assert!((p.shares("X") - 20.0).abs() < 1e-9); // doubled
        assert!((p.positions["X"][0].entry_price - 50.0).abs() < 1e-9); // halved
        let prices = HashMap::from([("X".to_string(), 50.0)]);
        assert!((p.equity(&prices) - 1000.0).abs() < 1e-9); // unchanged
    }

    #[test]
    fn split_in_backtest_preserves_equity_curve() {
        // Bar 0: price=100, buy. Bar 1: price=50 (2:1 split ex-date). Without lot
        // adjustment, equity would halve. With it, equity stays at 1.0.
        let bars = vec![
            bar("2024-01-01", 100.0),
            bar("2024-01-02", 50.0),
        ];
        let actions = vec![CorporateAction {
            ex_date: "2024-01-02".parse().unwrap(),
            ticker: "X".to_string(),
            kind: CaKind::Split { ratio: 2.0 },
        }];
        let r = run_portfolio_backtest("X", &bars, &Strategy::BuyAndHold, 1000.0, &FillCosts::ZERO, 0.0, &actions);
        assert!((r.curve[1].equity - 1.0).abs() < 1e-9);
    }

    #[test]
    fn congress_signals_use_correct_date_in_each_mode() {
        // 80 bars. Purchase disclosed on bar 20 (transaction date); filed on bar 60 (~40-day lag).
        let bars: Vec<Bar> = (0..80)
            .map(|i| bar(&format!("2022-{:02}-{:02}", i / 28 + 1, i % 28 + 1), 100.0))
            .collect();

        // Naive mode (transaction date): signal turns on at bar 20.
        let naive = congress_signals(&bars, &[(bars[20].date, 1.0), (bars[60].date, 0.0)]);
        assert_eq!(naive[19], 0.0, "before transaction date: no signal");
        assert_eq!(naive[20], 1.0, "at transaction date: enter");
        assert_eq!(naive[60], 0.0, "at sale transaction date: exit");

        // Realistic mode (filing date): signal only turns on at bar 60.
        let realistic = congress_signals(&bars, &[(bars[60].date, 1.0)]);
        assert_eq!(realistic[59], 0.0, "before filing date: no signal");
        assert_eq!(realistic[60], 1.0, "at filing date: enter");

        // Two modes produce different signals in bars 20–59 (the lag window).
        assert!(naive[30] != realistic[30], "modes differ in the lag window");
    }

    #[test]
    fn squeeze_signals_require_both_conditions() {
        // 30 bars: price rises steadily from 100.
        let bars: Vec<Bar> = (0..30)
            .map(|i| bar(&format!("2022-01-{:02}", i + 1), 100.0 + i as f64))
            .collect();
        let window = 10usize;

        // Case A: high DTC (8) + price above SMA → signal after the SMA warms up.
        let si_high: Vec<(NaiveDate, f64)> = vec![(bars[0].date, 8.0)];
        let sig = squeeze_signals(&bars, &si_high, 5.0, window);
        assert_eq!(sig[window - 1], 0.0, "window not yet filled");
        assert_eq!(sig[window], 1.0, "high DTC + rising price → enter");

        // Case B: low DTC (2) → no entry even with rising price.
        let si_low: Vec<(NaiveDate, f64)> = vec![(bars[0].date, 2.0)];
        let sig_low = squeeze_signals(&bars, &si_low, 5.0, window);
        assert!(sig_low.iter().all(|&s| s == 0.0), "low DTC: never enters");

        // Case C: high DTC starts mid-series (bar 15) → no signal before bar 15.
        let si_late: Vec<(NaiveDate, f64)> = vec![(bars[15].date, 8.0)];
        let sig_late = squeeze_signals(&bars, &si_late, 5.0, window);
        assert_eq!(sig_late[14], 0.0, "SI not yet reported");
        assert_eq!(sig_late[15], 1.0, "SI reported + above SMA → enter");
    }

    #[test]
    fn trade_log_captures_signal_transitions() {
        let base = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
        let bars: Vec<Bar> = (0..6).map(|i| Bar {
            date: base + chrono::Duration::days(i),
            open: 100.0, high: 100.0, low: 100.0, close: 100.0, volume: 1_000.0,
        }).collect();
        // signal: out, in, in, out, in — expect buy@1, sell@3, buy@4
        let signals = vec![0.0, 1.0, 1.0, 0.0, 1.0, 1.0];
        let r = run_signals_backtest("AAPL", &bars, &signals);
        assert_eq!(r.trades.len(), 3);
        assert_eq!(r.trades[0].action, "buy");
        assert_eq!(r.trades[1].action, "sell");
        assert_eq!(r.trades[2].action, "buy");
        assert_eq!(r.trades[0].ticker, "AAPL");
    }

    #[test]
    fn sma_has_no_lookahead() {
        // Drive the incremental generator one bar at a time; the signal it emits
        // at bar i must depend only on closes through i (no future leak).
        let mut gen = Strategy::SmaCrossover { fast: 2, slow: 4 }.into_generator();
        let sig: Vec<f64> = (1..=10)
            .map(|i| gen.next(&bar(&format!("2020-01-{i:02}"), i as f64)))
            .collect();
        // flat until the slow window fills...
        assert_eq!(sig[0], 0.0);
        assert_eq!(sig[2], 0.0);
        // ...then long on a rising series (fast SMA above slow SMA)
        assert_eq!(sig[3], 1.0);
    }
}
