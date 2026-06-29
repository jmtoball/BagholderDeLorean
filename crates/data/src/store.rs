//! DuckDB-backed cache for historic data. Embedded columnar store: one file,
//! SQL, fast range scans for backtests. OHLCV is a wide table; fundamentals is
//! tall (`ticker, period, metric, value`) so the metric set stays open-ended.

use crate::{download_cik_map, download_congress_trades, download_corporate_actions, download_cramer_calls, download_fred_series, download_fundamentals, download_ohlcv, download_sector_industry, download_short_interest, market_cap};
use crate::cramer::CramerCall;
use crate::finra::ShortInterest;
use crate::screen::DEFAULT_UNIVERSE;
use anyhow::{Context, Result};
use bagholder_core::{Bar, CaKind, CongressTrade, CorporateAction, Fundamental};
use chrono::NaiveDate;
use duckdb::{params, Connection};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS bars (
  ticker TEXT   NOT NULL,
  date   DATE   NOT NULL,
  open   DOUBLE, high DOUBLE, low DOUBLE, close DOUBLE, volume DOUBLE,
  PRIMARY KEY (ticker, date)
);
CREATE TABLE IF NOT EXISTS fundamentals (
  ticker      TEXT   NOT NULL,
  period      DATE   NOT NULL,
  metric      TEXT   NOT NULL,
  period_type TEXT   NOT NULL,  -- 'Q' or 'FY'
  value       DOUBLE,
  PRIMARY KEY (ticker, period, metric, period_type)
);
CREATE TABLE IF NOT EXISTS cik_map (
  ticker TEXT   PRIMARY KEY,
  cik    BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS macro_series (
  series_id TEXT NOT NULL,
  date      DATE NOT NULL,
  value     DOUBLE,
  PRIMARY KEY (series_id, date)
);
CREATE TABLE IF NOT EXISTS corporate_actions (
  ticker      TEXT   NOT NULL,
  ex_date     DATE   NOT NULL,
  action_type TEXT   NOT NULL,  -- 'split' or 'dividend'
  ratio       DOUBLE,           -- split ratio (new/old shares), NULL for dividends
  amount      DOUBLE,           -- dividend per share, NULL for splits
  PRIMARY KEY (ticker, ex_date, action_type)
);
-- Sentinel: tracks which tickers have had their CA fetched (handles zero-action case).
CREATE TABLE IF NOT EXISTS ca_fetched (ticker TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS congress_trades (
  member           TEXT NOT NULL,
  ticker           TEXT NOT NULL,
  transaction_date DATE NOT NULL,
  filing_date      DATE NOT NULL,
  trade_type       TEXT NOT NULL,
  amount_range     TEXT NOT NULL,
  PRIMARY KEY (member, ticker, transaction_date, trade_type, amount_range)
);
-- Sentinel: tracks which years have been fully fetched.
CREATE TABLE IF NOT EXISTS congress_fetched (year INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS cramer_calls (
  ticker TEXT NOT NULL,
  date   DATE NOT NULL,
  call   TEXT NOT NULL,  -- 'buy' or 'sell'
  PRIMARY KEY (ticker, date, call)
);
-- Any row here means the full dataset has been loaded.
CREATE TABLE IF NOT EXISTS cramer_fetched (sentinel INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS short_interest (
  ticker          TEXT NOT NULL,
  settlement_date DATE NOT NULL,
  short_qty       BIGINT NOT NULL,
  days_to_cover   DOUBLE NOT NULL,
  PRIMARY KEY (ticker, settlement_date)
);
-- Per-ticker sentinel; full history fetched once and cached.
CREATE TABLE IF NOT EXISTS short_interest_fetched (ticker TEXT PRIMARY KEY);
-- Yahoo instrument type per ticker ('EQUITY' / 'ETF' / 'MUTUALFUND' / …); rides
-- the ohlcv fetch. Absent row = unknown; callers treat unknown as 'not a fund'.
CREATE TABLE IF NOT EXISTS ticker_meta (
  ticker          TEXT PRIMARY KEY,
  instrument_type TEXT
);
-- Market-cap-filtered screener universe (≥$2B), backfilled by refresh_universe.
-- market_cap ≈ shares_outstanding × latest close. Refetch quarterly.
CREATE TABLE IF NOT EXISTS universe (
  ticker      TEXT PRIMARY KEY,
  sector      TEXT,
  industry    TEXT,
  market_cap  DOUBLE,
  computed_at DATE
);
";

/// Strip Stooq's exchange suffix and upper-case, so "AAPL.US" -> "AAPL" and
/// "BRK-B.US" -> "BRK-B", matching SEC's ticker spelling.
fn normalize_ticker(ticker: &str) -> String {
    ticker
        .rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(ticker)
        .to_uppercase()
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (and create/migrate) the store at `path`, e.g. "bagholder.duckdb".
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening duckdb at {path}"))?;
        Self::init(conn)
    }

    /// In-memory store, for tests.
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().context("opening in-memory duckdb")?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        // Migration: add filed_date to databases created before C1 landed.
        let _ = conn.execute_batch(
            "ALTER TABLE fundamentals ADD COLUMN IF NOT EXISTS filed_date DATE",
        );
        Ok(Self { conn })
    }

    /// Cached OHLCV: serve from the store, else download from Stooq and cache.
    /// ponytail: cache-forever once present — no freshness check. Add an
    /// incremental "download bars after the last stored date" path when you
    /// need up-to-date prices, not just historic backtests.
    pub fn ohlcv(&self, ticker: &str) -> Result<Vec<Bar>> {
        let cached = self.read_bars(ticker)?;
        if !cached.is_empty() {
            return Ok(cached);
        }
        let (bars, instrument_type) = download_ohlcv(ticker)?;
        self.write_bars(ticker, &bars)?;
        self.write_meta(ticker, instrument_type.as_deref())?;
        Ok(bars)
    }

    /// Yahoo instrument type for `ticker` ("EQUITY"/"ETF"/"MUTUALFUND"/…), or
    /// `None` when unknown. Serves the cache; on a miss it rides the ohlcv fetch
    /// (a no-op download when bars are already cached), then re-reads. `None` is
    /// a valid answer — callers must treat unknown as "not a fund", not an error.
    pub fn instrument_type(&self, ticker: &str) -> Result<Option<String>> {
        if let Some(t) = self.read_meta(ticker)? {
            return Ok(Some(t));
        }
        self.ohlcv(ticker)?;
        self.read_meta(ticker)
    }

    fn write_meta(&self, ticker: &str, instrument_type: Option<&str>) -> Result<()> {
        if let Some(t) = instrument_type {
            self.conn.execute(
                "INSERT OR REPLACE INTO ticker_meta VALUES (?, ?)",
                params![ticker, t],
            )?;
        }
        Ok(())
    }

    fn read_meta(&self, ticker: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT instrument_type FROM ticker_meta WHERE ticker = ?")?;
        let mut rows = stmt.query([ticker])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, Option<String>>(0)?),
            None => Ok(None),
        }
    }

    fn read_bars(&self, ticker: &str) -> Result<Vec<Bar>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, open, high, low, close, volume FROM bars \
             WHERE ticker = ? ORDER BY date",
        )?;
        let bars = stmt
            .query_map([ticker], |r| {
                Ok(Bar {
                    date: r.get(0)?,
                    open: r.get(1)?,
                    high: r.get(2)?,
                    low: r.get(3)?,
                    close: r.get(4)?,
                    volume: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(bars)
    }

    /// Full replace for the ticker (idempotent re-fetch). Wrapped in a
    /// transaction so a few thousand inserts commit once.
    fn write_bars(&self, ticker: &str, bars: &[Bar]) -> Result<()> {
        self.conn.execute("DELETE FROM bars WHERE ticker = ?", [ticker])?;
        self.conn.execute_batch("BEGIN")?;
        {
            let mut stmt = self
                .conn
                .prepare("INSERT INTO bars VALUES (?, ?, ?, ?, ?, ?, ?)")?;
            for b in bars {
                stmt.execute(params![
                    ticker, b.date, b.open, b.high, b.low, b.close, b.volume
                ])?;
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Cached fundamentals: serve from the store, else resolve the SEC CIK,
    /// download from EDGAR, cache, and return. ponytail: cache-forever (same as
    /// `ohlcv`) — refetch by deleting the ticker's rows.
    pub fn fundamentals(&self, ticker: &str) -> Result<Vec<Fundamental>> {
        let cached = self.read_fundamentals(ticker)?;
        if !cached.is_empty() {
            return Ok(cached);
        }
        let cik = self.cik(ticker)?;
        self.write_fundamentals(ticker, &download_fundamentals(cik)?)?;
        // Read back so callers always get the deduped, ordered set.
        self.read_fundamentals(ticker)
    }

    fn read_fundamentals(&self, ticker: &str) -> Result<Vec<Fundamental>> {
        let mut stmt = self.conn.prepare(
            "SELECT period, metric, period_type, value, filed_date FROM fundamentals \
             WHERE ticker = ? ORDER BY period, metric, period_type",
        )?;
        let funds = stmt
            .query_map([ticker], |r| {
                Ok(Fundamental {
                    period: r.get(0)?,
                    metric: r.get(1)?,
                    period_type: r.get(2)?,
                    value: r.get(3)?,
                    filed_date: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(funds)
    }

    fn write_fundamentals(&self, ticker: &str, funds: &[Fundamental]) -> Result<()> {
        self.conn
            .execute("DELETE FROM fundamentals WHERE ticker = ?", [ticker])?;
        self.conn.execute_batch("BEGIN")?;
        {
            // OR REPLACE: a period+metric+type can appear under several XBRL
            // tags or amended filings; last write wins.
            let mut stmt = self.conn.prepare(
                "INSERT OR REPLACE INTO fundamentals VALUES (?, ?, ?, ?, ?, ?)",
            )?;
            for f in funds {
                stmt.execute(params![
                    ticker, f.period, f.metric, f.period_type, f.value, f.filed_date
                ])?;
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Resolve a ticker to its SEC CIK, lazily populating the `cik_map` table
    /// from SEC's directory on first use.
    fn cik(&self, ticker: &str) -> Result<i64> {
        let sec_ticker = normalize_ticker(ticker);
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM cik_map", [], |r| r.get(0))?;
        if count == 0 {
            self.write_cik_map(&download_cik_map()?)?;
        }
        let mut stmt = self.conn.prepare("SELECT cik FROM cik_map WHERE ticker = ?")?;
        let mut rows = stmt.query_map([&sec_ticker], |r| r.get::<_, i64>(0))?;
        match rows.next() {
            Some(cik) => Ok(cik?),
            None => anyhow::bail!("no SEC CIK for ticker {sec_ticker}"),
        }
    }

    /// All US ticker symbols from SEC's directory, lazily populating `cik_map`
    /// on first use (same pattern as `cik()`).
    pub fn all_tickers(&self) -> Result<Vec<String>> {
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM cik_map", [], |r| r.get(0))?;
        if count == 0 {
            self.write_cik_map(&download_cik_map()?)?;
        }
        let mut stmt = self.conn.prepare("SELECT ticker FROM cik_map ORDER BY ticker")?;
        let tickers = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tickers)
    }

    /// Backfill the `universe` table: walk every `cik_map` ticker, approximate its
    /// market cap (latest shares outstanding × last close), keep names ≥ ~$2B, and
    /// tag sector/industry from Yahoo search. Returns the count kept. Skips names
    /// that error (delisted, no fundamentals, rate-limited). Throttled and meant to
    /// run off the request path (the `refresh-universe` CLI). ~1000+ network calls.
    /// ponytail: one-shot on demand; promote to a scheduled job if quarterly refetch
    /// becomes routine.
    pub fn refresh_universe(&self) -> Result<usize> {
        // Dual-class names share a CIK across rows, so each row undercounts the
        // company; keep the floor at 0.7×$2B to avoid dropping them. ponytail: pad
        // the floor rather than summing classes — simpler, slightly permissive.
        const FLOOR: f64 = 2.0e9 * 0.7;
        let mut kept = 0usize;
        for ticker in self.all_tickers()? {
            match self.universe_row(&ticker) {
                Ok(Some((cap, sector, industry))) if cap >= FLOOR => {
                    self.write_universe(&ticker, sector.as_deref(), industry.as_deref(), cap)?;
                    kept += 1;
                }
                Ok(_) => {}
                Err(e) => eprintln!("refresh_universe: skipping {ticker}: {e:#}"),
            }
            // Be polite to SEC/Yahoo.
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        Ok(kept)
    }

    /// Compute one ticker's `(market_cap, sector, industry)`, or `None` if it lacks
    /// a price or shares-outstanding figure. Caches prices/fundamentals as a side
    /// effect (same lazy path as the screener).
    fn universe_row(&self, ticker: &str) -> Result<Option<(f64, Option<String>, Option<String>)>> {
        let bars = self.ohlcv(ticker)?;
        let Some(last) = bars.last() else { return Ok(None); };
        let funds = self.fundamentals(ticker)?;
        // Most recent shares-outstanding figure.
        let shares = funds
            .iter()
            .filter(|f| f.metric == "shares_outstanding")
            .max_by_key(|f| f.period)
            .map(|f| f.value);
        let Some(shares) = shares.filter(|s| *s > 0.0) else { return Ok(None); };
        let cap = market_cap(shares, last.close);
        let (sector, industry) = download_sector_industry(ticker).unwrap_or((None, None));
        Ok(Some((cap, sector, industry)))
    }

    fn write_universe(&self, ticker: &str, sector: Option<&str>, industry: Option<&str>, cap: f64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO universe VALUES (?, ?, ?, ?, current_date)",
            params![ticker, sector, industry, cap],
        )?;
        Ok(())
    }

    /// The screening universe as `(ticker, industry)` pairs: the backfilled
    /// `universe` table, or the hardcoded `DEFAULT_UNIVERSE` seed when it's empty.
    /// Names without an industry tag fall back to their sector, then "Unknown".
    pub fn screen_universe(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ticker, COALESCE(industry, sector, 'Unknown') FROM universe ORDER BY ticker",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Ok(DEFAULT_UNIVERSE.iter().map(|(t, i)| (t.to_string(), i.to_string())).collect());
        }
        Ok(rows)
    }

    fn write_cik_map(&self, map: &[(String, i64)]) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        {
            let mut stmt = self
                .conn
                .prepare("INSERT OR REPLACE INTO cik_map VALUES (?, ?)")?;
            for (ticker, cik) in map {
                stmt.execute(params![ticker, cik])?;
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Cached corporate actions (splits + dividends) for a ticker.
    /// Uses `ca_fetched` as a sentinel so zero-action tickers aren't re-fetched.
    pub fn corporate_actions(&self, ticker: &str) -> Result<Vec<CorporateAction>> {
        let fetched: i64 = self.conn.query_row(
            "SELECT count(*) FROM ca_fetched WHERE ticker = ?", [ticker], |r| r.get(0),
        )?;
        if fetched > 0 {
            return self.read_actions(ticker);
        }
        let actions = download_corporate_actions(ticker)?;
        self.write_actions(ticker, &actions)?;
        self.conn.execute("INSERT OR IGNORE INTO ca_fetched VALUES (?)", [ticker])?;
        Ok(actions)
    }

    fn read_actions(&self, ticker: &str) -> Result<Vec<CorporateAction>> {
        let mut stmt = self.conn.prepare(
            "SELECT ex_date, action_type, ratio, amount FROM corporate_actions \
             WHERE ticker = ? ORDER BY ex_date",
        )?;
        let actions = stmt
            .query_map([ticker], |r| {
                let ex_date: NaiveDate = r.get(0)?;
                let action_type: String = r.get(1)?;
                let ratio: Option<f64> = r.get(2)?;
                let amount: Option<f64> = r.get(3)?;
                let kind = if action_type == "split" {
                    CaKind::Split { ratio: ratio.unwrap_or(1.0) }
                } else {
                    CaKind::Dividend { amount_per_share: amount.unwrap_or(0.0) }
                };
                Ok(CorporateAction { ex_date, ticker: String::new(), kind })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(actions.into_iter().map(|mut a| { a.ticker = ticker.to_owned(); a }).collect())
    }

    fn write_actions(&self, ticker: &str, actions: &[CorporateAction]) -> Result<()> {
        self.conn.execute("DELETE FROM corporate_actions WHERE ticker = ?", [ticker])?;
        self.conn.execute_batch("BEGIN")?;
        {
            let mut stmt = self.conn.prepare(
                "INSERT OR REPLACE INTO corporate_actions VALUES (?, ?, ?, ?, ?)",
            )?;
            for a in actions {
                let (action_type, ratio, amount) = match &a.kind {
                    CaKind::Split { ratio } => ("split", Some(*ratio), None),
                    CaKind::Dividend { amount_per_share } => ("dividend", None, Some(*amount_per_share)),
                };
                stmt.execute(params![ticker, a.ex_date, action_type, ratio, amount])?;
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Cached congressional PTR trades for `year`. Downloads on first access;
    /// ponytail: cache-forever — re-fetch by deleting rows for the year and the sentinel.
    pub fn congress_trades(&self, year: u32) -> Result<Vec<CongressTrade>> {
        let fetched: i64 = self.conn.query_row(
            "SELECT count(*) FROM congress_fetched WHERE year = ?",
            [year],
            |r| r.get(0),
        )?;
        if fetched > 0 {
            return self.read_congress_trades(year);
        }
        let trades = download_congress_trades(year)?;
        self.write_congress_trades(&trades)?;
        self.conn.execute("INSERT OR IGNORE INTO congress_fetched VALUES (?)", [year])?;
        Ok(trades)
    }

    fn read_congress_trades(&self, year: u32) -> Result<Vec<CongressTrade>> {
        let mut stmt = self.conn.prepare(
            "SELECT member, ticker, transaction_date, filing_date, trade_type, amount_range \
             FROM congress_trades \
             WHERE cast(strftime('%Y', filing_date) as integer) = ? \
             ORDER BY filing_date, member, ticker",
        )?;
        let rows = stmt
            .query_map([year], |r| {
                Ok(CongressTrade {
                    member: r.get(0)?,
                    ticker: r.get(1)?,
                    transaction_date: r.get(2)?,
                    filing_date: r.get(3)?,
                    trade_type: r.get(4)?,
                    amount_range: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn write_congress_trades(&self, trades: &[CongressTrade]) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        {
            let mut stmt = self.conn.prepare(
                "INSERT OR IGNORE INTO congress_trades VALUES (?, ?, ?, ?, ?, ?)",
            )?;
            for t in trades {
                stmt.execute(params![
                    t.member,
                    t.ticker,
                    t.transaction_date,
                    t.filing_date,
                    t.trade_type,
                    t.amount_range
                ])?;
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Cramer calls for a specific ticker, loading the full dataset on first access.
    /// ponytail: cache-forever; the upstream dataset is frozen ~2016–2022.
    pub fn cramer_calls(&self, ticker: &str) -> Result<Vec<CramerCall>> {
        let fetched: i64 = self.conn.query_row(
            "SELECT count(*) FROM cramer_fetched", [], |r| r.get(0),
        )?;
        if fetched == 0 {
            let calls = download_cramer_calls()?;
            self.write_cramer_calls(&calls)?;
            self.conn.execute("INSERT OR IGNORE INTO cramer_fetched VALUES (1)", [])?;
        }
        let mut stmt = self.conn.prepare(
            "SELECT ticker, date, call FROM cramer_calls WHERE ticker = ? ORDER BY date",
        )?;
        let rows = stmt
            .query_map([ticker], |r| {
                Ok(CramerCall {
                    ticker: r.get(0)?,
                    date: r.get(1)?,
                    call: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn write_cramer_calls(&self, calls: &[CramerCall]) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        {
            let mut stmt = self
                .conn
                .prepare("INSERT OR IGNORE INTO cramer_calls VALUES (?, ?, ?)")?;
            for c in calls {
                stmt.execute(params![c.ticker, c.date, c.call])?;
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// FINRA biweekly short interest for a ticker; fetched once and cached.
    pub fn short_interest(&self, ticker: &str) -> Result<Vec<ShortInterest>> {
        let fetched: i64 = self.conn.query_row(
            "SELECT count(*) FROM short_interest_fetched WHERE ticker = ?",
            [ticker], |r| r.get(0),
        )?;
        if fetched == 0 {
            let records = download_short_interest(ticker)?;
            self.write_short_interest(&records)?;
            self.conn.execute(
                "INSERT OR IGNORE INTO short_interest_fetched VALUES (?)", [ticker],
            )?;
        }
        let mut stmt = self.conn.prepare(
            "SELECT ticker, settlement_date, short_qty, days_to_cover \
             FROM short_interest WHERE ticker = ? ORDER BY settlement_date",
        )?;
        let rows = stmt
            .query_map([ticker], |r| {
                Ok(ShortInterest {
                    ticker: r.get(0)?,
                    settlement_date: r.get(1)?,
                    short_qty: r.get(2)?,
                    days_to_cover: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn write_short_interest(&self, records: &[ShortInterest]) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        {
            let mut stmt = self.conn.prepare(
                "INSERT OR IGNORE INTO short_interest VALUES (?, ?, ?, ?)",
            )?;
            for r in records {
                stmt.execute(params![r.ticker, r.settlement_date, r.short_qty, r.days_to_cover])?;
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Cached FRED macro series (e.g. "T10Y2Y", "CPIAUCSL").
    /// Downloads on first access; ponytail: cache-forever like OHLCV.
    pub fn macro_series(&self, series_id: &str) -> Result<Vec<(NaiveDate, f64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT date, value FROM macro_series WHERE series_id = ? ORDER BY date")?;
        let cached: Vec<(NaiveDate, f64)> = stmt
            .query_map([series_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        if !cached.is_empty() { return Ok(cached); }

        let rows = download_fred_series(series_id)?;
        self.conn.execute_batch("BEGIN")?;
        {
            let mut ins = self
                .conn
                .prepare("INSERT OR REPLACE INTO macro_series VALUES (?, ?, ?)")?;
            for (date, val) in &rows {
                ins.execute(params![series_id, date, val])?;
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn bar(d: &str, c: f64) -> Bar {
        Bar {
            date: d.parse::<NaiveDate>().unwrap(),
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 0.0,
        }
    }

    #[test]
    fn write_then_read_roundtrips_in_order() {
        let s = Store::in_memory().unwrap();
        s.write_bars("AAPL.US", &[bar("2020-01-03", 7.25), bar("2020-01-02", 7.1)])
            .unwrap();
        let got = s.read_bars("AAPL.US").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].date, "2020-01-02".parse::<NaiveDate>().unwrap()); // sorted
        assert_eq!(got[1].close, 7.25);
    }

    #[test]
    fn rewrite_replaces_rather_than_duplicates() {
        let s = Store::in_memory().unwrap();
        let bars = [bar("2020-01-02", 7.1)];
        s.write_bars("AAPL.US", &bars).unwrap();
        s.write_bars("AAPL.US", &bars).unwrap();
        assert_eq!(s.read_bars("AAPL.US").unwrap().len(), 1);
    }

    #[test]
    fn instrument_type_serves_cached_meta_else_none() {
        let s = Store::in_memory().unwrap();
        // Simulate what ohlcv persists after a download.
        s.write_bars("VOO", &[bar("2020-01-02", 300.0)]).unwrap();
        s.write_meta("VOO", Some("ETF")).unwrap();
        assert_eq!(s.instrument_type("VOO").unwrap(), Some("ETF".to_string()));

        // Bars cached but no meta row (older cache / sparse symbol): instrument_type
        // re-rides ohlcv (a no-op since bars exist), stays None — never errors.
        s.write_bars("PLAIN", &[bar("2020-01-02", 50.0)]).unwrap();
        assert_eq!(s.instrument_type("PLAIN").unwrap(), None);
    }

    #[test]
    #[ignore] // network: SEC companyfacts + Yahoo (run with `--ignored`)
    fn refresh_universe_keeps_large_caps_with_sector() {
        let s = Store::in_memory().unwrap();
        // Seed cik_map directly so all_tickers() doesn't download the full directory.
        s.write_cik_map(&[("AAPL".into(), 320193), ("MSFT".into(), 789019)]).unwrap();
        let kept = s.refresh_universe().unwrap();
        assert!(kept >= 1, "expected ≥1 large-cap kept, got {kept}");
        let u = s.screen_universe().unwrap();
        let aapl = u.iter().find(|(t, _)| t == "AAPL").expect("AAPL present");
        assert_ne!(aapl.1, "Unknown", "AAPL should carry a sector/industry tag, got {}", aapl.1);
    }

    #[test]
    fn screen_universe_reads_table_else_falls_back_to_seed() {
        let s = Store::in_memory().unwrap();
        // Empty universe table → the DEFAULT_UNIVERSE seed.
        let seed = s.screen_universe().unwrap();
        assert!(seed.iter().any(|(t, _)| t == "AAPL"), "fallback seed present");

        // Populated table takes over; industry, else sector, else "Unknown".
        s.write_universe("ZZZZ", Some("Energy"), Some("Oil & Gas"), 5.0e9).unwrap();
        s.write_universe("YYYY", Some("Tech"), None, 9.0e9).unwrap();
        let u = s.screen_universe().unwrap();
        assert_eq!(u.len(), 2, "table replaces the seed");
        assert!(u.contains(&("ZZZZ".to_string(), "Oil & Gas".to_string())));
        assert!(u.contains(&("YYYY".to_string(), "Tech".to_string())), "sector fallback");
    }

    #[test]
    fn normalizes_stooq_tickers_to_sec_spelling() {
        assert_eq!(normalize_ticker("AAPL.US"), "AAPL");
        assert_eq!(normalize_ticker("brk-b.us"), "BRK-B");
        assert_eq!(normalize_ticker("MSFT"), "MSFT");
    }

    #[test]
    fn all_tickers_lists_warmed_cik_map() {
        let s = Store::in_memory().unwrap();
        // warm cik_map directly so all_tickers() reads it instead of downloading
        s.write_cik_map(&[("AAPL".into(), 320193), ("BRK-B".into(), 1067983)])
            .unwrap();
        let got = s.all_tickers().unwrap();
        assert!(got.contains(&"AAPL".to_string()));
        assert!(got.contains(&"BRK-B".to_string()));
    }

    #[test]
    fn fundamentals_roundtrip_and_dedup() {
        let s = Store::in_memory().unwrap();
        let f = |period: &str, metric: &str, pt: &str, value: f64| Fundamental {
            period: period.parse().unwrap(),
            metric: metric.into(),
            period_type: pt.into(),
            value,
            filed_date: None,
        };
        s.write_fundamentals(
            "AAPL.US",
            &[
                // same period+metric+type twice -> OR REPLACE keeps the last value
                f("2022-12-31", "revenue", "FY", 1.0),
                f("2022-12-31", "revenue", "FY", 2.0),
                // same end date but quarterly -> distinct row, not collapsed
                f("2022-12-31", "revenue", "Q", 0.5),
            ],
        )
        .unwrap();
        let got = s.read_fundamentals("AAPL.US").unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().any(|x| x.period_type == "FY" && x.value == 2.0));
        assert!(got.iter().any(|x| x.period_type == "Q" && x.value == 0.5));
    }
}
