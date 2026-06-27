//! DuckDB-backed cache for historic data. Embedded columnar store: one file,
//! SQL, fast range scans for backtests. OHLCV is a wide table; fundamentals is
//! tall (`ticker, period, metric, value`) so the metric set stays open-ended.

use crate::download_ohlcv;
use anyhow::{Context, Result};
use bagholder_core::Bar;
use duckdb::{params, Connection};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS bars (
  ticker TEXT   NOT NULL,
  date   DATE   NOT NULL,
  open   DOUBLE, high DOUBLE, low DOUBLE, close DOUBLE, volume DOUBLE,
  PRIMARY KEY (ticker, date)
);
CREATE TABLE IF NOT EXISTS fundamentals (
  ticker TEXT   NOT NULL,
  period DATE   NOT NULL,
  metric TEXT   NOT NULL,
  value  DOUBLE,
  PRIMARY KEY (ticker, period, metric)
);
";

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
}
