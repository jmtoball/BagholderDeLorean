//! Polygon.io minute-bar client (free tier: ~2yr history, 5 req/min).
//! API key required: set POLYGON_API_KEY env var (free signup at polygon.io).
//!
//! ponytail: per-ticker sentinel, no partial refresh. If data is stale, delete
//! the row from minute_bars_fetched and re-fetch. Add incremental sync later.

use anyhow::{Context, Result};
use bagholder_core::MinuteBar;
use chrono::NaiveDate;
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct PolyResp {
    results: Option<Vec<PolyBar>>,
    next_url: Option<String>,
}
#[derive(Deserialize)]
struct PolyBar { t: i64, o: f64, h: f64, l: f64, c: f64, v: f64 }

/// Download minute bars for `ticker` between `from` and `to` (inclusive).
/// Paginates automatically; sleeps 12.5 s between requests to stay under 5 req/min.
pub fn download_minute_bars(
    ticker: &str,
    api_key: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<MinuteBar>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("BagholderDeLorean jm@gedankenacker.de")
        .timeout(Duration::from_secs(30))
        .build()?;

    let mut bars = Vec::new();
    // First URL uses date range; next_url carries a cursor for subsequent pages.
    let mut url: Option<String> = Some(format!(
        "https://api.polygon.io/v2/aggs/ticker/{ticker}/range/1/minute/{from}/{to}\
         ?adjusted=true&sort=asc&limit=50000&apiKey={api_key}"
    ));

    while let Some(next) = url.take() {
        let resp: PolyResp = client
            .get(&next)
            .send()
            .with_context(|| format!("Polygon request for {ticker}"))?
            .error_for_status()?
            .json()
            .context("parsing Polygon JSON")?;

        if let Some(results) = resp.results {
            for r in results {
                let ts = chrono::DateTime::from_timestamp_millis(r.t)
                    .with_context(|| format!("invalid Polygon timestamp {}", r.t))?
                    .naive_utc();
                bars.push(MinuteBar { ts, open: r.o, high: r.h, low: r.l, close: r.c, volume: r.v });
            }
        }

        if let Some(next_url) = resp.next_url {
            url = Some(format!("{next_url}&apiKey={api_key}"));
            std::thread::sleep(Duration::from_millis(12_500)); // 5 req/min
        }
    }
    Ok(bars)
}

/// Parse a Polygon aggregates JSON string (for tests — avoids network calls).
pub(crate) fn parse_poly_json(json: &str) -> Result<Vec<MinuteBar>> {
    let resp: PolyResp = serde_json::from_str(json).context("parsing Polygon JSON")?;
    let mut bars = Vec::new();
    for r in resp.results.unwrap_or_default() {
        let ts = chrono::DateTime::from_timestamp_millis(r.t)
            .with_context(|| format!("invalid ts {}", r.t))?
            .naive_utc();
        bars.push(MinuteBar { ts, open: r.o, high: r.h, low: r.l, close: r.c, volume: r.v });
    }
    Ok(bars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_poly_json_extracts_bars() {
        // Timestamps: 2024-01-02 09:30 UTC ≈ 1704188400000 ms (approx)
        let json = r#"{
            "results": [
                {"t": 1704188400000, "o": 185.0, "h": 186.0, "l": 184.5, "c": 185.5, "v": 12345.0},
                {"t": 1704188460000, "o": 185.5, "h": 187.0, "l": 185.0, "c": 186.2, "v": 8900.0}
            ],
            "next_url": null
        }"#;
        let bars = parse_poly_json(json).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].open, 185.0);
        assert_eq!(bars[0].high, 186.0);
        assert!((bars[0].close - 185.5).abs() < 1e-9);
        assert_eq!(bars[0].volume, 12345.0);
        // Second bar is 60 seconds later.
        let diff = bars[1].ts - bars[0].ts;
        assert_eq!(diff.num_seconds(), 60);
    }

    #[test]
    fn parse_poly_json_handles_empty_results() {
        let json = r#"{"status": "OK", "resultsCount": 0}"#;
        let bars = parse_poly_json(json).unwrap();
        assert!(bars.is_empty());
    }
}
