//! Historic data clients. Blocking I/O — the API crate calls these off the
//! async runtime via `spawn_blocking`. Kept out of the WASM web crate.

use anyhow::{Context, Result};
use bagholder_core::Bar;
use chrono::NaiveDate;
use serde::Deserialize;

/// Daily OHLCV from Stooq (free, no API key). Ticker uses Stooq's suffix
/// format, e.g. "AAPL.US", "SPY.US", "BMW.DE".
pub fn fetch_ohlcv(ticker: &str) -> Result<Vec<Bar>> {
    let url = format!("https://stooq.com/q/d/l/?s={ticker}&i=d");
    let body = reqwest::blocking::get(&url)
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()?
        .text()?;
    parse_stooq_csv(&body)
}

#[derive(Debug, Deserialize)]
struct StooqRow {
    #[serde(rename = "Date")]
    date: NaiveDate,
    #[serde(rename = "Open")]
    open: f64,
    #[serde(rename = "High")]
    high: f64,
    #[serde(rename = "Low")]
    low: f64,
    #[serde(rename = "Close")]
    close: f64,
    #[serde(rename = "Volume")]
    volume: f64,
}

fn parse_stooq_csv(body: &str) -> Result<Vec<Bar>> {
    // Stooq answers a bad ticker with a plain "No data" line, not a CSV.
    if body.trim_start().starts_with("No data") {
        anyhow::bail!("stooq returned no data (unknown ticker?)");
    }
    let mut rdr = csv::Reader::from_reader(body.as_bytes());
    let mut bars = Vec::new();
    for rec in rdr.deserialize() {
        let r: StooqRow = rec.context("parsing stooq CSV row")?;
        bars.push(Bar {
            date: r.date,
            open: r.open,
            high: r.high,
            low: r.low,
            close: r.close,
            volume: r.volume,
        });
    }
    Ok(bars)
}

/// ponytail: fundamentals/earnings stub. Wire to SEC EDGAR company facts
/// (https://data.sec.gov) or Yahoo quoteSummary when the first strategy needs
/// valuations. Returns a typed error so callers fail loudly, not silently.
pub fn fetch_fundamentals(_ticker: &str) -> Result<()> {
    anyhow::bail!("fundamentals client not implemented yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stooq_csv() {
        let csv = "Date,Open,High,Low,Close,Volume\n\
                   2020-01-02,7.0,7.2,6.9,7.1,100\n\
                   2020-01-03,7.1,7.3,7.0,7.25,120\n";
        let bars = parse_stooq_csv(csv).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[1].close, 7.25);
    }

    #[test]
    fn rejects_no_data_response() {
        assert!(parse_stooq_csv("No data\n").is_err());
    }
}
