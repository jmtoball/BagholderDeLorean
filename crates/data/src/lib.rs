//! Historic data clients + a DuckDB-backed cache (`Store`). Blocking I/O — the
//! API crate calls these off the async runtime via `spawn_blocking`. Kept out
//! of the WASM web crate.

mod store;
pub use store::Store;

use std::collections::HashMap;

use anyhow::{Context, Result};
use bagholder_core::{Bar, Fundamental};
use chrono::NaiveDate;
use serde::Deserialize;

/// Daily OHLCV straight from Stooq (free, no API key). Ticker uses Stooq's
/// suffix format, e.g. "AAPL.US", "SPY.US", "BMW.DE". Prefer `Store::ohlcv`,
/// which caches these results; this is the raw network fetch.
pub fn download_ohlcv(ticker: &str) -> Result<Vec<Bar>> {
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

// --- SEC EDGAR fundamentals --------------------------------------------------

// SEC requires a descriptive User-Agent with contact info, else it 403s.
const SEC_UA: &str = "BagholderDeLorean jm@gedankenacker.de";

/// Canonical metric -> the us-gaap XBRL tag(s) it can appear under. Revenue in
/// particular moved tags over the years, so we try several and store under one
/// name. ponytail: a curated set, not all of XBRL — add rows as strategies need
/// more metrics.
const METRICS: &[(&str, &[&str])] = &[
    (
        "revenue",
        &[
            "RevenueFromContractWithCustomerExcludingAssessedTax",
            "Revenues",
            "SalesRevenueNet",
        ],
    ),
    ("net_income", &["NetIncomeLoss"]),
    ("eps_basic", &["EarningsPerShareBasic"]),
    ("assets", &["Assets"]),
    ("liabilities", &["Liabilities"]),
    ("equity", &["StockholdersEquity"]),
    ("shares_outstanding", &["CommonStockSharesOutstanding"]),
];

fn sec_get(url: &str) -> Result<String> {
    let body = reqwest::blocking::Client::builder()
        .user_agent(SEC_UA)
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()?
        .text()?;
    Ok(body)
}

/// SEC's full ticker -> CIK directory (~1 MB). Tickers come back upper-cased and
/// suffix-free, e.g. "AAPL", "BRK-B".
pub fn download_cik_map() -> Result<Vec<(String, i64)>> {
    parse_cik_map(&sec_get("https://www.sec.gov/files/company_tickers.json")?)
}

fn parse_cik_map(body: &str) -> Result<Vec<(String, i64)>> {
    #[derive(Deserialize)]
    struct Entry {
        cik_str: i64,
        ticker: String,
    }
    // The file is a JSON object keyed by row index: {"0": {...}, "1": {...}}.
    let rows: HashMap<String, Entry> = serde_json::from_str(body).context("parsing CIK map")?;
    Ok(rows.into_values().map(|e| (e.ticker, e.cik_str)).collect())
}

/// All curated fundamentals for a company, by SEC CIK. Prefer
/// `Store::fundamentals`, which resolves the CIK and caches.
pub fn download_fundamentals(cik: i64) -> Result<Vec<Fundamental>> {
    let url = format!("https://data.sec.gov/api/xbrl/companyfacts/CIK{cik:010}.json");
    parse_company_facts(&sec_get(&url)?)
}

fn parse_company_facts(body: &str) -> Result<Vec<Fundamental>> {
    #[derive(Deserialize)]
    struct Facts {
        #[serde(rename = "us-gaap", default)]
        us_gaap: HashMap<String, Concept>,
    }
    #[derive(Deserialize)]
    struct CompanyFacts {
        facts: Facts,
    }
    #[derive(Deserialize)]
    struct Concept {
        units: HashMap<String, Vec<Point>>,
    }
    #[derive(Deserialize)]
    struct Point {
        start: Option<NaiveDate>,
        end: NaiveDate,
        val: f64,
        #[serde(default)]
        form: String,
    }

    let facts: CompanyFacts = serde_json::from_str(body).context("parsing companyfacts JSON")?;
    let mut out = Vec::new();
    for (name, tags) in METRICS {
        for tag in *tags {
            let Some(concept) = facts.facts.us_gaap.get(*tag) else {
                continue;
            };
            for points in concept.units.values() {
                for p in points {
                    // Only periodic filings; skip 8-Ks and the like.
                    if !matches!(p.form.as_str(), "10-K" | "10-Q") {
                        continue;
                    }
                    // Classify by duration. Income facts carry a start date, so
                    // we can keep the clean ~quarter and ~year figures and drop
                    // the year-to-date cumulatives that share an end date.
                    let Some(period_type) = classify_period(p.start, p.end, &p.form) else {
                        continue;
                    };
                    out.push(Fundamental {
                        period: p.end,
                        metric: name.to_string(),
                        period_type: period_type.to_string(),
                        value: p.val,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Returns "Q", "FY", or `None` (drop). Duration facts (income statement) are
/// classified by length: ~quarter or ~year, the rest (YTD/semi-annual) dropped.
/// Instantaneous facts (balance sheet) have no start, so we classify by form.
fn classify_period(start: Option<NaiveDate>, end: NaiveDate, form: &str) -> Option<&'static str> {
    match start {
        Some(start) => match (end - start).num_days() {
            80..=100 => Some("Q"),
            350..=380 => Some("FY"),
            _ => None,
        },
        None if form == "10-K" => Some("FY"),
        None => Some("Q"),
    }
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

    #[test]
    fn parses_cik_map() {
        let json = r#"{"0":{"cik_str":320193,"ticker":"AAPL","title":"Apple Inc."},
                       "1":{"cik_str":789019,"ticker":"MSFT","title":"Microsoft"}}"#;
        let mut map = parse_cik_map(json).unwrap();
        map.sort();
        assert_eq!(map, vec![("AAPL".into(), 320193), ("MSFT".into(), 789019)]);
    }

    #[test]
    fn classify_period_by_duration_then_form() {
        let d = |s: &str| s.parse::<NaiveDate>().unwrap();
        assert_eq!(classify_period(Some(d("2022-01-01")), d("2022-03-31"), "10-Q"), Some("Q"));
        assert_eq!(classify_period(Some(d("2022-01-01")), d("2022-12-31"), "10-K"), Some("FY"));
        // year-to-date (6 months) is neither a clean quarter nor a year -> dropped
        assert_eq!(classify_period(Some(d("2022-01-01")), d("2022-06-30"), "10-Q"), None);
        // instantaneous balance-sheet facts: classified by form
        assert_eq!(classify_period(None, d("2022-12-31"), "10-K"), Some("FY"));
        assert_eq!(classify_period(None, d("2022-09-30"), "10-Q"), Some("Q"));
    }

    #[test]
    fn parses_company_facts_curated_metrics_only() {
        let json = r#"{
          "facts": { "us-gaap": {
            "NetIncomeLoss": { "units": { "USD": [
              {"start":"2022-01-01","end":"2022-12-31","val":1000,"form":"10-K"},
              {"start":"2022-07-01","end":"2022-09-30","val":250,"form":"10-Q"},
              {"start":"2022-01-01","end":"2022-06-30","val":600,"form":"10-Q"},
              {"start":"2022-04-01","end":"2022-06-30","val":99,"form":"8-K"}
            ]}},
            "SomeUnusedTag": { "units": { "USD": [
              {"start":"2022-01-01","end":"2022-12-31","val":42,"form":"10-K"}
            ]}}
          }}
        }"#;
        let funds = parse_company_facts(json).unwrap();
        // 8-K dropped, YTD (6mo) dropped, unlisted tag ignored -> FY + one quarter
        assert_eq!(funds.len(), 2);
        assert!(funds.iter().all(|f| f.metric == "net_income"));
        assert!(funds.iter().any(|f| f.value == 1000.0 && f.period_type == "FY"));
        assert!(funds.iter().any(|f| f.value == 250.0 && f.period_type == "Q"));
    }
}
