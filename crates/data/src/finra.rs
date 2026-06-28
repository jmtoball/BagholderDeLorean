//! FINRA Equity Short Interest client.
//! Endpoint: POST https://api.finra.org/data/group/otcMarket/name/equityShortInterest
//! Free, no auth, biweekly settlement dates, archives to ~2014.
//!
//! ponytail: field names from FINRA developer docs (2024-06); verify at
//! https://developer.finra.org if queries start returning empty.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde_json::Value;
use std::time::Duration;

#[derive(Debug)]
pub struct ShortInterest {
    pub ticker: String,
    pub settlement_date: NaiveDate,
    pub short_qty: i64,
    pub days_to_cover: f64,
}

/// Fetch all historical short-interest records for `ticker` from FINRA.
pub fn download_short_interest(ticker: &str) -> Result<Vec<ShortInterest>> {
    let url = "https://api.finra.org/data/group/otcMarket/name/equityShortInterest";
    let body = serde_json::json!({
        "limit": 5000,
        "offset": 0,
        "fields": [
            "symbolCode", "settlementDate",
            "currentShortPositionQuantity", "daysToCoverQuantity"
        ],
        "compareFilters": [
            {"fieldName": "symbolCode", "fieldValue": ticker, "compareType": "EQUAL"}
        ],
        "sortFields": [{"fieldName": "settlementDate", "sortType": "ASC"}]
    });

    let records: Vec<Value> = reqwest::blocking::Client::builder()
        .user_agent("BagholderDeLorean jm@gedankenacker.de")
        .timeout(Duration::from_secs(30))
        .build()?
        .post(url)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .with_context(|| format!("FINRA request for {ticker}"))?
        .error_for_status()?
        .json()
        .context("parsing FINRA JSON")?;

    parse_finra_response(&records)
}

pub(crate) fn parse_finra_response(records: &[Value]) -> Result<Vec<ShortInterest>> {
    let mut out = Vec::new();
    for rec in records {
        let ticker = rec["symbolCode"].as_str().unwrap_or("").trim().to_uppercase();
        let date_str = rec["settlementDate"].as_str().unwrap_or("").trim();
        let short_qty = rec["currentShortPositionQuantity"].as_i64().unwrap_or(0);
        let days_to_cover = rec["daysToCoverQuantity"].as_f64().unwrap_or(0.0);

        if ticker.is_empty() || date_str.is_empty() {
            continue;
        }
        let settlement_date: NaiveDate = date_str
            .parse()
            .with_context(|| format!("bad date '{date_str}' for {ticker}"))?;
        out.push(ShortInterest { ticker, settlement_date, short_qty, days_to_cover });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_finra_response_extracts_fields() {
        let json: Vec<Value> = serde_json::from_str(r#"[
            {
                "symbolCode": "AAPL",
                "settlementDate": "2024-01-15",
                "currentShortPositionQuantity": 123456789,
                "daysToCoverQuantity": 1.23
            },
            {
                "symbolCode": "AAPL",
                "settlementDate": "2024-02-01",
                "currentShortPositionQuantity": 987654321,
                "daysToCoverQuantity": 2.5
            }
        ]"#).unwrap();

        let si = parse_finra_response(&json).unwrap();
        assert_eq!(si.len(), 2);
        assert_eq!(si[0].ticker, "AAPL");
        assert_eq!(si[0].settlement_date, NaiveDate::from_ymd_opt(2024, 1, 15).unwrap());
        assert_eq!(si[0].short_qty, 123456789);
        assert!((si[0].days_to_cover - 1.23).abs() < 0.001);
        assert_eq!(si[1].short_qty, 987654321);
    }

    #[test]
    fn parse_finra_response_skips_empty_rows() {
        let json: Vec<Value> = serde_json::from_str(r#"[
            {"symbolCode": "", "settlementDate": "2024-01-15",
             "currentShortPositionQuantity": 1, "daysToCoverQuantity": 1.0},
            {"symbolCode": "GME", "settlementDate": "2024-01-29",
             "currentShortPositionQuantity": 50000000, "daysToCoverQuantity": 3.1}
        ]"#).unwrap();

        let si = parse_finra_response(&json).unwrap();
        assert_eq!(si.len(), 1);
        assert_eq!(si[0].ticker, "GME");
    }
}
