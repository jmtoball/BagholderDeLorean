//! DuckDB-backed cache for historic data. Embedded columnar store: one file,
//! SQL, fast range scans for backtests. OHLCV is a wide table; fundamentals is
//! tall (`ticker, period, metric, value`) so the metric set stays open-ended.

use crate::{download_cik_map, download_fundamentals, download_ohlcv};
use anyhow::{Context, Result};
use bagholder_core::{Bar, Fundamental};
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
        let bars = download_ohlcv(ticker)?;
        self.write_bars(ticker, &bars)?;
        Ok(bars)
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
            "SELECT period, metric, period_type, value FROM fundamentals \
             WHERE ticker = ? ORDER BY period, metric, period_type",
        )?;
        let funds = stmt
            .query_map([ticker], |r| {
                Ok(Fundamental {
                    period: r.get(0)?,
                    metric: r.get(1)?,
                    period_type: r.get(2)?,
                    value: r.get(3)?,
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
            let mut stmt = self
                .conn
                .prepare("INSERT OR REPLACE INTO fundamentals VALUES (?, ?, ?, ?, ?)")?;
            for f in funds {
                stmt.execute(params![ticker, f.period, f.metric, f.period_type, f.value])?;
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
    fn normalizes_stooq_tickers_to_sec_spelling() {
        assert_eq!(normalize_ticker("AAPL.US"), "AAPL");
        assert_eq!(normalize_ticker("brk-b.us"), "BRK-B");
        assert_eq!(normalize_ticker("MSFT"), "MSFT");
    }

    #[test]
    fn fundamentals_roundtrip_and_dedup() {
        let s = Store::in_memory().unwrap();
        let f = |period: &str, metric: &str, pt: &str, value: f64| Fundamental {
            period: period.parse().unwrap(),
            metric: metric.into(),
            period_type: pt.into(),
            value,
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
