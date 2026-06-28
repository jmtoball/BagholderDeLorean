//! House Clerk PTR (Periodic Transaction Report) ingestion.
//!
//! Data flow: bulk ZIP → FD XML (filing metadata) → per-DocID PDF → extracted text
//! → transaction rows → DuckDB. Only FilingType="P" (PTR) records are processed.
//! Text-layer PDFs are parsed; scanned/OCR-only filings are skipped and counted.

use anyhow::{Context, Result};
use bagholder_core::CongressTrade;
use chrono::NaiveDate;
use std::io::Read;

const CLERK_UA: &str = "BagholderDeLorean jm@gedankenacker.de";

/// Download and parse House Clerk PTR filings for `year`.
/// ponytail: one HTTP call per PTR filing (hundreds per year) — no rate limit
/// currently needed for a single dev run; add a small sleep if House Clerk 429s.
pub fn download_congress_trades(year: u32) -> Result<Vec<CongressTrade>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(CLERK_UA)
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    // 1. Download the annual bulk ZIP.
    let zip_url = format!(
        "https://disclosures-clerk.house.gov/public_disc/financial-pdfs/{}FD.zip",
        year
    );
    let zip_bytes: Vec<u8> = client
        .get(&zip_url)
        .send()
        .with_context(|| format!("fetching {zip_url}"))?
        .error_for_status()?
        .bytes()?
        .to_vec();

    // 2. Unzip and parse the FD XML for PTR filing stubs.
    let xml = {
        let cursor = std::io::Cursor::new(&zip_bytes);
        let mut archive = zip::ZipArchive::new(cursor).context("opening FD ZIP")?;
        let xml_name = format!("{}FD.xml", year);
        let mut file = archive
            .by_name(&xml_name)
            .with_context(|| format!("no {xml_name} in ZIP"))?;
        let mut s = String::new();
        file.read_to_string(&mut s).context("reading FD XML")?;
        s
    };
    let stubs = parse_fd_xml(&xml)?;

    // 3. For each PTR, download the PDF and extract trades.
    let mut trades = Vec::new();
    let mut skipped = 0usize;

    for (member, filing_date, doc_id) in stubs {
        let base = doc_id.trim_end_matches(".pdf");
        let pdf_url = format!(
            "https://disclosures-clerk.house.gov/public_disc/ptr-pdfs/{year}/{base}.pdf"
        );
        let pdf_res = client
            .get(&pdf_url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes());

        match pdf_res {
            Ok(bytes) => {
                let text = pdf_extract::extract_text_from_mem(&bytes).unwrap_or_default();
                if text.trim().len() < 50 {
                    // Scanned / image-only PDF — no usable text layer.
                    skipped += 1;
                } else {
                    match parse_ptr_text(&text, &member, filing_date) {
                        Ok(t) if t.is_empty() => skipped += 1,
                        Ok(t) => trades.extend(t),
                        Err(_) => skipped += 1,
                    }
                }
            }
            Err(_) => skipped += 1,
        }
    }

    if skipped > 0 {
        eprintln!(
            "congress_trades({year}): skipped {skipped} filings (scanned or unparseable)"
        );
    }
    Ok(trades)
}

/// Parse the FD bulk XML into `(member_name, filing_date, doc_id)` for PTR filings only.
/// ponytail: assumes House Clerk XML element names from the 2012+ eFD system;
/// handles both `<Member>` and `<FinancialDisclosureMember>` wrappers.
pub(crate) fn parse_fd_xml(xml: &str) -> Result<Vec<(String, NaiveDate, String)>> {
    let doc = roxmltree::Document::parse(xml).context("parsing FD XML")?;
    let mut out = Vec::new();

    for node in doc.descendants() {
        let tag = node.tag_name().name();
        if tag != "Member" && tag != "FinancialDisclosureMember" {
            continue;
        }

        let child_text = |name: &str| -> Option<String> {
            node.children()
                .find(|n| n.tag_name().name().eq_ignore_ascii_case(name))
                .and_then(|n| n.text())
                .map(|t| t.trim().to_owned())
                .filter(|s| !s.is_empty())
        };

        // Only PTR filings (FilingType = "P").
        if !child_text("FilingType")
            .as_deref()
            .map(|v| v.eq_ignore_ascii_case("P"))
            .unwrap_or(false)
        {
            continue;
        }

        let first = child_text("First").unwrap_or_default();
        let last = child_text("Last").unwrap_or_default();
        let member = format!("{first} {last}").trim().to_owned();

        let doc_id = match child_text("DocID") {
            Some(id) => id,
            None => continue,
        };

        let date_str = child_text("FilingDate").unwrap_or_default();
        let filing_date = parse_mdy_date(&date_str)
            .with_context(|| format!("bad FilingDate '{date_str}' for {member}"))?;

        out.push((member, filing_date, doc_id));
    }
    Ok(out)
}

/// Parse PTR PDF-extracted text into trade rows.
/// ponytail: line-scan heuristic — works for standard eFD-generated PDFs;
/// PDFs where the table spans multiple lines per row may produce zero results
/// (those are counted as skipped). Improve with a 2-line window join if
/// coverage drops below acceptable for a given member set.
pub(crate) fn parse_ptr_text(
    text: &str,
    member: &str,
    filing_date: NaiveDate,
) -> Result<Vec<CongressTrade>> {
    let mut trades = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // A transaction row needs at minimum a ticker (UPPER) in parens + a date.
        let Some(ticker) = find_ticker(line) else {
            continue;
        };
        let Some(transaction_date) = find_date(line) else {
            continue;
        };
        let trade_type = classify_type(line);
        let amount_range = find_amount(line).unwrap_or_else(|| "unknown".to_owned());

        trades.push(CongressTrade {
            member: member.to_owned(),
            ticker,
            transaction_date,
            filing_date,
            trade_type,
            amount_range,
        });
    }
    Ok(trades)
}

/// Find the first `(AAPL)` style ticker: `(` + 1–5 ASCII uppercase letters + `)`.
fn find_ticker(line: &str) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    for i in 0..chars.len() {
        if chars[i] != '(' {
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < chars.len() && chars[end].is_ascii_uppercase() {
            end += 1;
        }
        let len = end - start;
        if (1..=5).contains(&len) && end < chars.len() && chars[end] == ')' {
            return Some(chars[start..end].iter().collect());
        }
    }
    None
}

/// Find the first standalone date token in "MM/DD/YYYY" or "MM-DD-YYYY" format.
fn find_date(line: &str) -> Option<NaiveDate> {
    for token in line.split_whitespace() {
        if token.len() < 8 || token.len() > 10 {
            continue;
        }
        let b = token.as_bytes();
        // Allow single-digit month/day: detect sep at index 1 or 2.
        let sep_pos = if b.get(1).copied() == Some(b'/') || b.get(1).copied() == Some(b'-') {
            1usize
        } else if b.get(2).copied() == Some(b'/') || b.get(2).copied() == Some(b'-') {
            2usize
        } else {
            continue;
        };
        let sep = b[sep_pos] as char;
        // Split into 3 parts by sep.
        let parts: Vec<&str> = token.splitn(3, sep).collect();
        if parts.len() != 3 {
            continue;
        }
        let (Ok(m), Ok(d), Ok(y)): (Result<u32, _>, Result<u32, _>, Result<i32, _>) = (
            parts[0].parse(),
            parts[1].parse(),
            parts[2].parse(),
        ) else {
            continue;
        };
        if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
            return Some(date);
        }
    }
    None
}

/// Classify transaction type from free-form text.
fn classify_type(line: &str) -> String {
    let lower = line.to_lowercase();
    if lower.contains("sale (partial)") || lower.contains("sale(partial)") || lower.contains("partial sale") {
        "sale_partial".to_owned()
    } else if lower.contains("sale") {
        "sale".to_owned()
    } else if lower.contains("purchase") {
        "purchase".to_owned()
    } else if lower.contains("exchange") {
        "exchange".to_owned()
    } else {
        "unknown".to_owned()
    }
}

/// Extract the first `$X - $Y` amount range, up to 40 chars from the first `$`.
fn find_amount(line: &str) -> Option<String> {
    let pos = line.find('$')?;
    let chunk = &line[pos..line.len().min(pos + 40)];
    // Walk right until we've passed the second number.
    let mut dollar_count = 0u8;
    let mut last_digit = 0usize;
    for (i, c) in chunk.char_indices() {
        if c == '$' {
            dollar_count += 1;
        }
        if dollar_count >= 2 && c.is_ascii_digit() {
            last_digit = i;
        }
        if dollar_count >= 2 && last_digit > 0 && !c.is_ascii_digit() && c != ',' {
            break;
        }
    }
    if last_digit == 0 {
        // Only one `$` found — still a valid single value.
        let end = chunk.find(|c: char| !c.is_ascii_digit() && c != ',' && c != '$')
            .unwrap_or(chunk.len());
        Some(chunk[..end].trim().to_owned()).filter(|s| !s.is_empty())
    } else {
        Some(chunk[..=last_digit].trim().to_owned())
    }
}

/// Parse "MM/DD/YYYY" or "MM-DD-YYYY" into a `NaiveDate` (used for FilingDate in XML).
fn parse_mdy_date(s: &str) -> Result<NaiveDate> {
    let sep = if s.contains('/') { '/' } else { '-' };
    let parts: Vec<&str> = s.splitn(3, sep).collect();
    anyhow::ensure!(parts.len() == 3, "expected MM/DD/YYYY, got '{s}'");
    let m: u32 = parts[0].parse().with_context(|| format!("bad month in '{s}'"))?;
    let d: u32 = parts[1].parse().with_context(|| format!("bad day in '{s}'"))?;
    let y: i32 = parts[2].parse().with_context(|| format!("bad year in '{s}'"))?;
    NaiveDate::from_ymd_opt(y, m, d).with_context(|| format!("invalid date from '{s}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fd_xml_keeps_only_ptr_filings() {
        let xml = r#"<?xml version="1.0"?>
<FinancialDisclosure>
  <Members>
    <Member>
      <First>Nancy</First><Last>Pelosi</Last>
      <FilingDate>01/11/2023</FilingDate>
      <FilingType>P</FilingType>
      <DocID>20023287</DocID>
    </Member>
    <Member>
      <First>John</First><Last>Smith</Last>
      <FilingDate>02/01/2023</FilingDate>
      <FilingType>A</FilingType>
      <DocID>20023288</DocID>
    </Member>
  </Members>
</FinancialDisclosure>"#;

        let stubs = parse_fd_xml(xml).unwrap();
        assert_eq!(stubs.len(), 1, "annual filing should be dropped");
        assert_eq!(stubs[0].0, "Nancy Pelosi");
        assert_eq!(stubs[0].1, "2023-01-11".parse::<NaiveDate>().unwrap());
        assert_eq!(stubs[0].2, "20023287");
    }

    #[test]
    fn parse_ptr_text_extracts_trades_with_both_dates() {
        let text = "Transactions\n\
                    Asset  Type  Date  Amount\n\
                    Apple Inc (AAPL)  Purchase  12/15/2022  $15,001 - $50,000\n\
                    Microsoft Corp (MSFT)  Sale  11/30/2022  $1,001 - $15,000\n\
                    Some Fund (SPXL)  Sale (Partial)  10/03/2022  $100,001 - $250,000\n";
        let filing_date: NaiveDate = "2023-01-11".parse().unwrap();

        let trades = parse_ptr_text(text, "Nancy Pelosi", filing_date).unwrap();
        assert_eq!(trades.len(), 3);

        assert_eq!(trades[0].ticker, "AAPL");
        assert_eq!(trades[0].trade_type, "purchase");
        assert_eq!(trades[0].transaction_date, "2022-12-15".parse::<NaiveDate>().unwrap());
        assert_eq!(trades[0].filing_date, filing_date);
        assert!(trades[0].amount_range.contains("15,001"), "amount: {}", trades[0].amount_range);

        assert_eq!(trades[1].ticker, "MSFT");
        assert_eq!(trades[1].trade_type, "sale");

        assert_eq!(trades[2].ticker, "SPXL");
        assert_eq!(trades[2].trade_type, "sale_partial");
    }

    #[test]
    fn parse_ptr_text_empty_for_scanned_indicator() {
        // Short / empty text → scanned PDF, no trades.
        let filing_date: NaiveDate = "2023-01-11".parse().unwrap();
        let result = parse_ptr_text("", "Nancy Pelosi", filing_date).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_mdy_date_handles_padded_and_unpadded() {
        assert_eq!(
            parse_mdy_date("01/11/2023").unwrap(),
            "2023-01-11".parse::<NaiveDate>().unwrap()
        );
        assert_eq!(
            parse_mdy_date("1/9/2023").unwrap(),
            "2023-01-09".parse::<NaiveDate>().unwrap()
        );
    }
}
