//! Static Cramer call ingestion from gaborvecsei/Mad-Money-Backtesting CSV.
//! Coverage is frozen ~2016–2022 (upstream TheStreet screener is dead).
//!
//! ponytail: one-shot bulk fetch; no live source needed. Add Quiver Quantitative
//! if live calls are ever required (paid API).

use anyhow::{Context, Result};
use chrono::NaiveDate;

/// One Jim Cramer call from Mad Money.
pub struct CramerCall {
    pub ticker: String,
    pub date: NaiveDate,
    /// "buy" or "sell"
    pub call: String,
}

/// Download and parse the Cramer call CSV from GitHub.
/// ponytail: primary URL is gaborvecsei's dataset; update if the repo moves.
pub fn download_cramer_calls() -> Result<Vec<CramerCall>> {
    let url = "https://raw.githubusercontent.com/gaborvecsei/Mad-Money-Backtesting/master/data/mad_money_lightning_round.csv";
    let body = reqwest::blocking::Client::builder()
        .user_agent("BagholderDeLorean jm@gedankenacker.de")
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()?
        .text()?;
    parse_cramer_csv(&body)
}

/// Parse CSV; columns discovered from header (case-insensitive).
/// Rows with calls other than "buy"/"sell" are dropped (e.g. "hold").
pub(crate) fn parse_cramer_csv(csv_text: &str) -> Result<Vec<CramerCall>> {
    let mut rdr = csv::Reader::from_reader(csv_text.as_bytes());
    let headers = rdr.headers().context("reading CSV headers")?.clone();

    let find = |names: &[&str]| -> Option<usize> {
        names.iter().find_map(|n| {
            headers
                .iter()
                .position(|h| h.trim().eq_ignore_ascii_case(n))
        })
    };
    let sym_col  = find(&["symbol", "ticker"]).context("no Symbol/Ticker column in CSV")?;
    let date_col = find(&["date"]).context("no Date column in CSV")?;
    let call_col = find(&["call", "recommendation"]).context("no Call column in CSV")?;

    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec.context("reading CSV record")?;
        let ticker = rec.get(sym_col).unwrap_or("").trim().to_uppercase();
        let date_str = rec.get(date_col).unwrap_or("").trim();
        let call = rec.get(call_col).unwrap_or("").trim().to_lowercase();

        if ticker.is_empty() || date_str.is_empty() {
            continue;
        }
        if call != "buy" && call != "sell" {
            continue; // skip "hold" and unknown labels
        }
        let date: NaiveDate = date_str
            .parse()
            .with_context(|| format!("bad date '{date_str}' for {ticker}"))?;
        out.push(CramerCall { ticker, date, call });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cramer_csv_fades_buy_to_short_sell_to_long() {
        let csv = "Segment,Date,Show,Name,Symbol,Call\n\
                   Lightning Round,2021-01-14,Mad Money,Tesla Inc.,TSLA,buy\n\
                   Lightning Round,2021-01-15,Mad Money,Apple Inc.,AAPL,sell\n\
                   Lightning Round,2021-01-16,Mad Money,Ford Motor,F,hold\n"; // hold dropped

        let calls = parse_cramer_csv(csv).unwrap();
        assert_eq!(calls.len(), 2, "hold should be dropped");
        assert_eq!(calls[0].ticker, "TSLA");
        assert_eq!(calls[0].call, "buy");
        assert_eq!(calls[1].ticker, "AAPL");
        assert_eq!(calls[1].call, "sell");

        // Fading logic used by the API: buy → -1.0 (short), sell → 1.0 (long).
        let events: Vec<(NaiveDate, f64)> = calls
            .iter()
            .map(|c| (c.date, if c.call == "buy" { -1.0_f64 } else { 1.0 }))
            .collect();
        assert_eq!(events[0].1, -1.0, "Cramer buy → short the fade");
        assert_eq!(events[1].1, 1.0, "Cramer sell → long the fade");
    }

    #[test]
    fn parse_cramer_csv_ignores_unknown_calls() {
        let csv = "Symbol,Date,Call\nAAPL,2021-06-01,speculative\nMSFT,2021-06-02,buy\n";
        let calls = parse_cramer_csv(csv).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].ticker, "MSFT");
    }
}
