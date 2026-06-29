//! Historic data clients + a DuckDB-backed cache (`Store`). Blocking I/O — the
//! API crate calls these off the async runtime via `spawn_blocking`. Kept out
//! of the WASM web crate.

mod congress;
mod cramer;
mod finra;
mod screen;
mod store;
pub use congress::download_congress_trades;
pub use cramer::{download_cramer_calls, CramerCall};
pub use finra::{download_short_interest, ShortInterest};
pub use screen::{low_pe, DEFAULT_UNIVERSE};
pub use store::Store;

use std::collections::HashMap;

use anyhow::{Context, Result};
use bagholder_core::{Bar, CaKind, CorporateAction, Fundamental};
use chrono::NaiveDate;
use serde::Deserialize;

/// Daily OHLCV from Yahoo Finance's chart API (free, no key). `symbol` is a
/// plain Yahoo ticker, e.g. "AAPL", "BRK-B". Close is split/dividend-adjusted
/// for correct return math; open/high/low are raw. Prefer `Store::ohlcv`, which
/// caches this; this is the raw network fetch.
///
/// ponytail: the v8 chart endpoint is undocumented but stable and keyless. If
/// Yahoo starts rate-limiting bursts, add a small backoff or a paid feed.
/// Human-facing "no data" message — names the ticker, no URL or HTTP jargon.
/// Used for both a 404 and an empty/error chart response so the UI shows the
/// same honest line however Yahoo signals the miss.
fn no_data_msg(symbol: &str) -> String {
    format!("Couldn't load data for \"{symbol}\" \u{2014} it may be an unknown or delisted ticker.")
}

/// Returns the daily bars plus Yahoo's `meta.instrumentType` ("EQUITY", "ETF",
/// "MUTUALFUND", …) when present — it rides the same chart fetch, no extra call.
pub fn download_ohlcv(symbol: &str) -> Result<(Vec<Bar>, Option<String>)> {
    // Explicit period1/period2 over `range=max`: the latter makes Yahoo silently
    // downsample to monthly bars. period1=0 (epoch) pulls full daily history.
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{symbol}\
         ?period1=0&period2=9999999999&interval=1d"
    );
    let resp = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; BagholderDeLorean/0.1)")
        .timeout(std::time::Duration::from_secs(20))
        .build()?
        .get(&url)
        .send()
        // Context names the ticker, not the URL — the raw reqwest error (which
        // embeds the URL) stays in the source chain, off the Display the API shows.
        .with_context(|| format!("requesting market data for {symbol}"))?;
    // Yahoo answers unknown symbols with 404; map it before error_for_status,
    // whose Display leaks the full URL.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("{}", no_data_msg(symbol));
    }
    let body = resp
        .error_for_status()
        .with_context(|| format!("fetching market data for {symbol}"))?
        .text()?;
    parse_yahoo_chart(symbol, &body)
}

fn parse_yahoo_chart(symbol: &str, body: &str) -> Result<(Vec<Bar>, Option<String>)> {
    #[derive(Deserialize)]
    struct Resp {
        chart: Chart,
    }
    #[derive(Deserialize)]
    struct Chart {
        result: Option<Vec<ChartResult>>,
        error: Option<serde_json::Value>,
    }
    #[derive(Deserialize)]
    struct ChartResult {
        timestamp: Option<Vec<i64>>,
        indicators: Indicators,
        // Yahoo's meta block — we keep only the instrument type; the rest is discarded.
        #[serde(default)]
        meta: Meta,
    }
    #[derive(Deserialize, Default)]
    struct Meta {
        #[serde(rename = "instrumentType")]
        instrument_type: Option<String>,
    }
    #[derive(Deserialize)]
    struct Indicators {
        quote: Vec<Quote>,
        #[serde(default)]
        adjclose: Vec<AdjClose>,
    }
    #[derive(Deserialize)]
    struct Quote {
        open: Vec<Option<f64>>,
        high: Vec<Option<f64>>,
        low: Vec<Option<f64>>,
        close: Vec<Option<f64>>,
        volume: Vec<Option<f64>>,
    }
    #[derive(Deserialize)]
    struct AdjClose {
        adjclose: Vec<Option<f64>>,
    }

    let resp: Resp = serde_json::from_str(body).context("parsing yahoo chart JSON")?;
    if resp.chart.error.is_some() {
        anyhow::bail!("{}", no_data_msg(symbol));
    }
    let result = resp
        .chart
        .result
        .and_then(|mut rs| rs.pop())
        .ok_or_else(|| anyhow::anyhow!("{}", no_data_msg(symbol)))?;
    let instrument_type = result.meta.instrument_type.clone();
    let ts = result.timestamp.unwrap_or_default();
    let q = result
        .indicators
        .quote
        .into_iter()
        .next()
        .context("yahoo returned no quote series")?;
    let adj = result.indicators.adjclose.into_iter().next().map(|a| a.adjclose);

    let mut bars = Vec::with_capacity(ts.len());
    for i in 0..ts.len() {
        // Skip rows with any missing core value (holidays, the in-progress day).
        let (Some(open), Some(high), Some(low), Some(close)) = (
            q.open.get(i).copied().flatten(),
            q.high.get(i).copied().flatten(),
            q.low.get(i).copied().flatten(),
            q.close.get(i).copied().flatten(),
        ) else {
            continue;
        };
        let date = chrono::DateTime::from_timestamp(ts[i], 0)
            .context("invalid timestamp from yahoo")?
            .date_naive();
        // Adjusted close for split/div-correct returns; fall back to raw close.
        let close = adj
            .as_ref()
            .and_then(|a| a.get(i).copied().flatten())
            .unwrap_or(close);
        bars.push(Bar {
            date,
            open,
            high,
            low,
            close,
            volume: q.volume.get(i).copied().flatten().unwrap_or(0.0),
        });
    }
    if bars.is_empty() {
        anyhow::bail!("yahoo returned no usable bars");
    }
    Ok((bars, instrument_type))
}

// --- Corporate actions (splits + dividends) ----------------------------------

/// Download splits and dividends for `symbol` from Yahoo Finance's chart API.
/// Same endpoint as `download_ohlcv`; the `events` parameter returns actions.
pub fn download_corporate_actions(symbol: &str) -> Result<Vec<CorporateAction>> {
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{symbol}\
         ?period1=0&period2=9999999999&interval=1d&events=split,dividend"
    );
    let body = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; BagholderDeLorean/0.1)")
        .timeout(std::time::Duration::from_secs(20))
        .build()?
        .get(&url)
        .send()
        .with_context(|| format!("requesting corporate actions for {symbol}"))?
        .error_for_status()?
        .text()?;
    parse_yahoo_events(symbol, &body)
}

fn parse_yahoo_events(ticker: &str, body: &str) -> Result<Vec<CorporateAction>> {
    #[derive(Deserialize, Default)]
    struct Events {
        #[serde(default)]
        splits: std::collections::HashMap<String, SplitRow>,
        #[serde(default)]
        dividends: std::collections::HashMap<String, DivRow>,
    }
    #[derive(Deserialize)]
    struct SplitRow { date: i64, numerator: f64, denominator: f64 }
    #[derive(Deserialize)]
    struct DivRow { date: i64, amount: f64 }
    #[derive(Deserialize)]
    struct ChartResult { #[serde(default)] events: Events }
    #[derive(Deserialize)]
    struct Chart { result: Option<Vec<ChartResult>> }
    #[derive(Deserialize)]
    struct Resp { chart: Chart }

    let resp: Resp = serde_json::from_str(body).context("parsing yahoo events JSON")?;
    let events = resp.chart.result
        .and_then(|mut rs| rs.pop())
        .map(|r| r.events)
        .unwrap_or_default();

    let mut out = Vec::new();
    for row in events.splits.values() {
        if row.denominator == 0.0 { continue; }
        let ex_date = chrono::DateTime::from_timestamp(row.date, 0)
            .context("invalid split timestamp")?
            .date_naive();
        out.push(CorporateAction {
            ex_date,
            ticker: ticker.to_owned(),
            kind: CaKind::Split { ratio: row.numerator / row.denominator },
        });
    }
    for row in events.dividends.values() {
        let ex_date = chrono::DateTime::from_timestamp(row.date, 0)
            .context("invalid dividend timestamp")?
            .date_naive();
        out.push(CorporateAction {
            ex_date,
            ticker: ticker.to_owned(),
            kind: CaKind::Dividend { amount_per_share: row.amount },
        });
    }
    out.sort_by_key(|a| a.ex_date);
    Ok(out)
}

// --- FRED macro series -------------------------------------------------------

/// Download a FRED time-series via the public CSV endpoint (no API key).
/// `series_id` is e.g. "T10Y2Y", "CPIAUCSL". Missing observations (`.`) are dropped.
pub fn download_fred_series(series_id: &str) -> Result<Vec<(NaiveDate, f64)>> {
    let url = format!("https://fred.stlouisfed.org/graph/fredgraph.csv?id={series_id}");
    let body = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; BagholderDeLorean/0.1)")
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .get(&url)
        .send()
        .with_context(|| format!("requesting FRED series {series_id}"))?
        .error_for_status()?
        .text()?;
    parse_fred_csv(&body)
}

fn parse_fred_csv(body: &str) -> Result<Vec<(NaiveDate, f64)>> {
    let mut rows = Vec::new();
    for line in body.lines().skip(1) {
        let mut it = line.splitn(2, ',');
        let date_str = it.next().unwrap_or("").trim();
        let val_str = it.next().unwrap_or("").trim();
        if val_str == "." || val_str.is_empty() || date_str.is_empty() { continue; }
        let date: NaiveDate = date_str.parse().with_context(|| format!("bad FRED date: {date_str}"))?;
        let val: f64 = val_str.parse().with_context(|| format!("bad FRED value: {val_str}"))?;
        rows.push((date, val));
    }
    Ok(rows)
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
        .timeout(std::time::Duration::from_secs(30))
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

/// Sector + industry for `symbol` from Yahoo Finance's search endpoint. Either
/// field is `None` when Yahoo doesn't classify the name (ETFs, funds, some ADRs).
pub fn download_sector_industry(symbol: &str) -> Result<(Option<String>, Option<String>)> {
    let url = format!("https://query1.finance.yahoo.com/v1/finance/search?q={symbol}");
    let body = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; BagholderDeLorean/0.1)")
        .timeout(std::time::Duration::from_secs(20))
        .build()?
        .get(&url)
        .send()
        .with_context(|| format!("searching Yahoo for {symbol}"))?
        .error_for_status()?
        .text()?;
    parse_sector_industry(symbol, &body)
}

fn parse_sector_industry(symbol: &str, body: &str) -> Result<(Option<String>, Option<String>)> {
    #[derive(Deserialize)]
    struct Quote {
        symbol: Option<String>,
        sector: Option<String>,
        industry: Option<String>,
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        quotes: Vec<Quote>,
    }
    let resp: Resp = serde_json::from_str(body).context("parsing Yahoo search JSON")?;
    // Prefer the quote whose symbol matches; else the first that carries a sector.
    let pick = resp
        .quotes
        .iter()
        .find(|q| q.symbol.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(symbol)))
        .or_else(|| resp.quotes.iter().find(|q| q.sector.is_some()));
    Ok(pick.map_or((None, None), |q| (q.sector.clone(), q.industry.clone())))
}

/// Market capitalisation ≈ shares outstanding × price. An approximation — see
/// the dual-class caveat at the `universe` filter.
pub fn market_cap(shares_outstanding: f64, price: f64) -> f64 {
    shares_outstanding * price
}

/// Universe inclusion floor: $2B, padded to 0.7× so dual-class names (whose
/// shares split across `cik_map` rows, each undercounting) aren't dropped.
pub const UNIVERSE_FLOOR: f64 = 2.0e9 * 0.7;

/// Compute one name's `(market_cap, sector, industry)` straight from the network,
/// without touching the store — so the in-process backfill holds the DB lock only
/// for the upsert, never across these slow fetches. `None` when the name lacks a
/// price or a shares-outstanding figure (ETFs, funds, freshly-listed).
///
/// ponytail: discards the downloaded bars/fundamentals rather than caching them,
/// so the first `/api/screen` after a backfill re-fetches the universe (the
/// "cold universe is slow, cheap thereafter" model still holds — it just isn't
/// pre-warmed). Caching here would warm the screen but balloon the DB with every
/// kept name's full history; warm it under the upsert lock if screen latency
/// matters (#89).
pub fn compute_universe_row(ticker: &str, cik: i64) -> Result<Option<(f64, Option<String>, Option<String>)>> {
    let (bars, _) = download_ohlcv(ticker)?;
    let Some(last) = bars.last() else { return Ok(None) };
    let shares = download_fundamentals(cik)?
        .into_iter()
        .filter(|f| f.metric == "shares_outstanding")
        .max_by_key(|f| f.period)
        .map(|f| f.value);
    let Some(shares) = shares.filter(|s| *s > 0.0) else { return Ok(None) };
    let cap = market_cap(shares, last.close);
    let (sector, industry) = download_sector_industry(ticker).unwrap_or((None, None));
    Ok(Some((cap, sector, industry)))
}

/// All curated fundamentals for a company, by SEC CIK. Prefer
/// `Store::fundamentals`, which resolves the CIK and caches.
pub fn download_fundamentals(cik: i64) -> Result<Vec<Fundamental>> {
    // Fetch filing dates from submissions first; facts reference them by accn.
    let filing_dates = download_submissions_dates(cik)?;
    let url = format!("https://data.sec.gov/api/xbrl/companyfacts/CIK{cik:010}.json");
    parse_company_facts(&sec_get(&url)?, &filing_dates)
}

/// Accession-number → filing date from SEC submissions (recent filings only).
/// ponytail: fetches only `filings.recent` — typically covers ~5-10 years.
/// Facts from older filings get `filed_date: None` and fall back to period end.
pub(crate) fn download_submissions_dates(cik: i64) -> Result<HashMap<String, NaiveDate>> {
    #[derive(Deserialize)]
    struct Subs {
        filings: Filings,
    }
    #[derive(Deserialize)]
    struct Filings {
        recent: Recent,
    }
    #[derive(Deserialize)]
    struct Recent {
        #[serde(rename = "accessionNumber")]
        accession_number: Vec<String>,
        #[serde(rename = "filingDate")]
        filing_date: Vec<String>,
    }

    let url = format!("https://data.sec.gov/submissions/CIK{cik:010}.json");
    let body = sec_get(&url)?;
    let subs: Subs = serde_json::from_str(&body).context("parsing submissions JSON")?;
    let r = subs.filings.recent;
    Ok(r.accession_number
        .into_iter()
        .zip(r.filing_date)
        .filter_map(|(accn, date_str)| date_str.parse::<NaiveDate>().ok().map(|d| (accn, d)))
        .collect())
}

fn parse_company_facts(
    body: &str,
    filing_dates: &HashMap<String, NaiveDate>,
) -> Result<Vec<Fundamental>> {
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
        #[serde(default)]
        accn: String,
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
                    if !matches!(p.form.as_str(), "10-K" | "10-Q") {
                        continue;
                    }
                    let Some(period_type) = classify_period(p.start, p.end, &p.form) else {
                        continue;
                    };
                    out.push(Fundamental {
                        period: p.end,
                        filed_date: filing_dates.get(&p.accn).copied(),
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
    fn parses_yahoo_chart_using_adjclose_and_skipping_gaps() {
        // Third row has a null close (e.g. the in-progress day) -> dropped.
        let json = r#"{"chart":{"error":null,"result":[{
          "timestamp":[1577923200,1578009600,1578095600],
          "indicators":{
            "quote":[{"open":[7.0,7.1,7.3],"high":[7.2,7.3,7.4],
                      "low":[6.9,7.0,7.1],"close":[7.1,7.25,null],"volume":[100,120,130]}],
            "adjclose":[{"adjclose":[6.5,6.6,null]}]
          }}]}}"#;
        let (bars, _) = parse_yahoo_chart("AAPL", json).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].date, "2020-01-02".parse::<NaiveDate>().unwrap());
        assert_eq!(bars[1].close, 6.6); // adjusted close, not raw 7.25
    }

    #[test]
    fn parses_instrument_type_from_meta_when_present() {
        let with_meta = r#"{"chart":{"error":null,"result":[{
          "meta":{"instrumentType":"ETF"},
          "timestamp":[1577923200],
          "indicators":{
            "quote":[{"open":[7.0],"high":[7.2],"low":[6.9],"close":[7.1],"volume":[100]}],
            "adjclose":[{"adjclose":[6.5]}]
          }}]}}"#;
        let (_, it) = parse_yahoo_chart("VOO", with_meta).unwrap();
        assert_eq!(it, Some("ETF".to_string()));

        // Same payload, no meta block -> None (older cache rows / sparse symbols).
        let no_meta = r#"{"chart":{"error":null,"result":[{
          "timestamp":[1577923200],
          "indicators":{
            "quote":[{"open":[7.0],"high":[7.2],"low":[6.9],"close":[7.1],"volume":[100]}],
            "adjclose":[{"adjclose":[6.5]}]
          }}]}}"#;
        let (_, it) = parse_yahoo_chart("AAPL", no_meta).unwrap();
        assert_eq!(it, None);
    }


    #[test]
    fn rejects_unknown_symbol_with_clean_message() {
        // Yahoo's unknown-symbol shape (null result + error). The message must
        // name the ticker and leak no URL/host — it surfaces straight to the UI.
        let json = r#"{"chart":{"result":null,"error":{"code":"Not Found","description":"No data found, symbol may be delisted"}}}"#;
        let msg = parse_yahoo_chart("XXXXINVALID", json).unwrap_err().to_string();
        assert!(msg.contains("XXXXINVALID"), "should name the ticker: {msg}");
        let lower = msg.to_lowercase();
        assert!(!lower.contains("http"),   "should not leak a URL: {msg}");
        assert!(!lower.contains("query1"), "should not leak the Yahoo host: {msg}");
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
    fn market_cap_multiplies_shares_by_price() {
        assert!((market_cap(15_000_000_000.0, 200.0) - 3.0e12).abs() < 1.0);
    }

    #[test]
    fn parses_sector_industry_for_matching_symbol() {
        let json = r#"{"quotes":[
            {"symbol":"AAPL","sector":"Technology","industry":"Consumer Electronics"},
            {"symbol":"AAPL.MX","sector":"Other","industry":"Other"}]}"#;
        assert_eq!(
            parse_sector_industry("AAPL", json).unwrap(),
            (Some("Technology".into()), Some("Consumer Electronics".into()))
        );
        // No quotes / unclassified (e.g. an ETF) → (None, None), no error.
        assert_eq!(parse_sector_industry("VOO", r#"{"quotes":[]}"#).unwrap(), (None, None));
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
              {"start":"2022-01-01","end":"2022-12-31","val":1000,"form":"10-K","accn":"0000001-22-000001"},
              {"start":"2022-07-01","end":"2022-09-30","val":250,"form":"10-Q","accn":"0000001-22-000002"},
              {"start":"2022-01-01","end":"2022-06-30","val":600,"form":"10-Q","accn":"0000001-22-000003"},
              {"start":"2022-04-01","end":"2022-06-30","val":99,"form":"8-K","accn":"0000001-22-000004"}
            ]}},
            "SomeUnusedTag": { "units": { "USD": [
              {"start":"2022-01-01","end":"2022-12-31","val":42,"form":"10-K","accn":"0000001-22-000001"}
            ]}}
          }}
        }"#;
        // Without filing dates: filed_date should be None.
        let funds = parse_company_facts(json, &HashMap::new()).unwrap();
        // 8-K dropped, YTD (6mo) dropped, unlisted tag ignored -> FY + one quarter
        assert_eq!(funds.len(), 2);
        assert!(funds.iter().all(|f| f.metric == "net_income"));
        assert!(funds.iter().any(|f| f.value == 1000.0 && f.period_type == "FY"));
        assert!(funds.iter().any(|f| f.value == 250.0 && f.period_type == "Q"));
        assert!(funds.iter().all(|f| f.filed_date.is_none()));
    }

    #[test]
    fn parse_company_facts_resolves_filing_date() {
        let json = r#"{
          "facts": { "us-gaap": {
            "EarningsPerShareBasic": { "units": { "USD/shares": [
              {"start":"2022-07-01","end":"2022-09-30","val":1.29,"form":"10-Q","accn":"0000320193-22-000108"}
            ]}}
          }}
        }"#;
        let mut dates = HashMap::new();
        dates.insert(
            "0000320193-22-000108".to_string(),
            "2022-10-28".parse::<NaiveDate>().unwrap(),
        );
        let funds = parse_company_facts(json, &dates).unwrap();
        assert_eq!(funds.len(), 1);
        assert_eq!(funds[0].filed_date, Some("2022-10-28".parse::<NaiveDate>().unwrap()));
        assert_eq!(funds[0].period, "2022-09-30".parse::<NaiveDate>().unwrap());
    }

    #[test]
    fn parse_fred_csv_skips_missing() {
        let csv = "DATE,T10Y2Y\n2023-01-01,1.23\n2023-01-02,.\n2023-01-03,0.95\n";
        let rows = parse_fred_csv(csv).unwrap();
        assert_eq!(rows.len(), 2);
        assert!((rows[0].1 - 1.23).abs() < 1e-9);
        assert!((rows[1].1 - 0.95).abs() < 1e-9);
    }
}
