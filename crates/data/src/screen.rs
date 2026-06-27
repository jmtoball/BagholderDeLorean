//! Fundamental screeners. They read the cached store, so the first call for a
//! cold universe is slow (downloads prices + companyfacts for every name) and
//! cheap thereafter.

use std::collections::HashMap;

use crate::Store;
use anyhow::Result;
use bagholder_core::{pe_ttm, Candidate};
use chrono::NaiveDate;

/// Default screening universe: a few liquid names per industry, so an
/// industry-relative median is meaningful. ponytail: hardcoded with industries
/// baked in — swap for a fetched S&P 500 list (+ SEC SIC lookup for industry)
/// when you outgrow it.
pub const DEFAULT_UNIVERSE: &[(&str, &str)] = &[
    ("AAPL", "Technology"),
    ("MSFT", "Technology"),
    ("GOOGL", "Technology"),
    ("NVDA", "Technology"),
    ("JPM", "Banks"),
    ("BAC", "Banks"),
    ("WFC", "Banks"),
    ("C", "Banks"),
    ("WMT", "Retail"),
    ("TGT", "Retail"),
    ("COST", "Retail"),
    ("HD", "Retail"),
    ("XOM", "Energy"),
    ("CVX", "Energy"),
    ("COP", "Energy"),
    ("SLB", "Energy"),
    ("JNJ", "Pharma"),
    ("PFE", "Pharma"),
    ("MRK", "Pharma"),
    ("ABBV", "Pharma"),
    ("F", "Autos"),
    ("GM", "Autos"),
    ("TSLA", "Autos"),
];

/// Rank the universe by P/E relative to each name's industry median, cheapest
/// first. Names missing a price or ≥4 quarters of EPS are skipped.
pub fn low_pe(store: &Store, universe: &[(&str, &str)], limit: usize) -> Result<Vec<Candidate>> {
    // 1. each company's trailing P/E. A single name's data hiccup (delisted,
    // rate-limited, missing EPS) shouldn't sink the whole screen — skip it.
    let mut raw: Vec<(String, String, f64)> = Vec::new();
    for (ticker, industry) in universe {
        match company_pe(store, ticker) {
            Ok(Some(pe)) => raw.push((ticker.to_string(), industry.to_string(), pe)),
            Ok(None) => {}
            Err(e) => eprintln!("screen: skipping {ticker}: {e:#}"),
        }
    }

    // 2. median P/E per industry
    let mut by_industry: HashMap<&str, Vec<f64>> = HashMap::new();
    for (_, industry, pe) in &raw {
        by_industry.entry(industry).or_default().push(*pe);
    }
    let median_pe: HashMap<&str, f64> =
        by_industry.iter().map(|(k, v)| (*k, median(v))).collect();

    // 3. relative P/E, ranked cheapest-vs-peers first
    let mut out: Vec<Candidate> = raw
        .iter()
        .map(|(ticker, industry, pe)| {
            let m = median_pe[industry.as_str()];
            Candidate {
                ticker: ticker.clone(),
                industry: industry.clone(),
                pe: *pe,
                industry_median_pe: m,
                relative_pe: pe / m,
            }
        })
        .collect();
    out.sort_by(|a, b| a.relative_pe.partial_cmp(&b.relative_pe).unwrap());
    out.truncate(limit);
    Ok(out)
}

fn company_pe(store: &Store, ticker: &str) -> Result<Option<f64>> {
    let bars = store.ohlcv(ticker)?;
    let Some(last) = bars.last() else {
        return Ok(None);
    };
    let funds = store.fundamentals(ticker)?;
    let mut eps: Vec<(NaiveDate, f64)> = funds
        .iter()
        .filter(|f| f.metric == "eps_basic" && f.period_type == "Q")
        .map(|f| (f.period, f.value))
        .collect();
    eps.sort_by(|a, b| b.0.cmp(&a.0)); // most recent first
    let recent: Vec<f64> = eps.into_iter().map(|(_, v)| v).collect();
    Ok(pe_ttm(last.close, &recent))
}

fn median(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    match n {
        0 => f64::NAN,
        _ if n % 2 == 1 => v[n / 2],
        _ => (v[n / 2 - 1] + v[n / 2]) / 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_handles_even_and_odd_lengths() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
    }
}
