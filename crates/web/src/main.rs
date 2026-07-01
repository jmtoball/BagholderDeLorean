//! Bagholder DeLorean — two-concern backtesting UI.
//! Stock selection (what to trade) × Trade action (when to get in/out).
//! Presets bypass the two-panel structure when selection and action are inseparable.
//!
//! ponytail: inline-style px are tokenized (`--space-*`/`--text-*`/`--radius-*`/
//!   `--border-*`) only where a token matches exactly. Remaining raw px are
//!   deliberate: off-grid fine-tuning (7/9/11/13/14/18px, 11.5/13.5px) with no
//!   token, or layout/illustration geometry (chart W/H, max-widths, absolute
//!   offsets, image sizes) that isn't on the spacing scale — don't force a token.

pub mod components;

use std::collections::{HashMap, HashSet};

use bagholder_core::{BacktestResult, Candidate, EquityPoint, PeHistory, TaxSystem, TradeEvent};
use chrono::{Datelike, NaiveDate};
use leptos::*;
use serde::de::DeserializeOwned;

use components::{BdBadge, BdButton, BdCallout, BdCard, BdCheckbox, BdInput, BdSelect, BdSiteFooter, BdStat, BdSwitch, BdTabs, Chip, FooterLink, Icon, Overline, RateChips, TabItem};

// ─── Chart geometry ───────────────────────────────────────────────────────────
const W: f64   = 720.0;
const H: f64   = 240.0;
const PAD: f64 = 8.0;
const OVERLAY_COLORS: &[&str] = &[
    "var(--accent)", "var(--denim-500)", "var(--gain)",
    "var(--warn)",   "var(--rust-400)",   "var(--loss)",
];

// ─── Strategy / screen data ───────────────────────────────────────────────────

fn is_preset(id: &str) -> bool {
    matches!(id, "pairs" | "riskparity" | "sectorrot" | "cycle")
}
fn action_label(id: &str) -> &'static str {
    match id {
        "buyhold"    => "Buy & Hold",
        "sma"        => "SMA Crossover",
        "golden"     => "Golden Cross / Death Cross",
        "btfd"       => "BTFD (Buy The Dip)",
        "meanrev"    => "Regime-Filtered Mean Reversion",
        "pairs"      => "Pairs / Stat-Arb",
        "riskparity" => "Risk Parity",
        "sectorrot"  => "Momentum Sector Rotation",
        "cycle"      => "Economic-Cycle Rotation",
        "cramer"         => "Inverse Cramer",
        "congress"       => "Congressional Copy-Trade",
        "short_squeeze"  => "Short Squeeze",
        _                => "",
    }
}
fn action_rationale(id: &str) -> &'static str {
    match id {
        "buyhold"    => "Buy it, forget it, touch grass. Quietly beats most active traders — and all of their stress.",
        "sma"        => "Go long when a fast MA crosses above a slow one. Trend-following with an on/off switch.",
        "golden"     => "The famous 50/200-day cross. A preset of SMA Crossover that pundits will not shut up about.",
        "btfd"       => "When RSI craters, you pounce. Sometimes a discount, sometimes a falling knife. Bring gloves.",
        "meanrev"    => "Buy dips and sell rips, but only when the market regime says it is reasonably safe to.",
        "pairs"      => "Trade the spread between two names. Selection and signal are the relationship itself.",
        "riskparity" => "A self-contained multi-asset mix weighted by inverse volatility. Boring on purpose.",
        "sectorrot"  => "Rotate into the top-N sectors by trailing return. Selection and action move together.",
        "cycle"      => "Tilt toward the sectors that tend to lead each phase of the macro cycle.",
        "cramer"        => "Selection is Cramer's picks; the action is to fade them. Inseparable, by design.",
        "congress"      => "Mirror disclosed politician trades. Naively spectacular — until you wait for the filing date.",
        "short_squeeze" => "Enter when short interest is high and price is rising. Exit when momentum fades.",
        _               => "",
    }
}
fn action_to_strategy(id: &str) -> &'static str {
    match id {
        "sma" | "golden" => "sma_crossover",
        "btfd"           => "buy_the_dip",
        "meanrev"        => "regime_mean_reversion",
        _                => "buy_and_hold",
    }
}
fn is_meme(id: &str) -> bool { matches!(id, "cramer" | "congress" | "short_squeeze") }
/// A "screen" run (rank a universe) rather than a single-ticker run. Presets
/// define their own selection, so they're never screen runs. Single source of
/// truth shared by `run()`, the Run-button label, and the loading copy.
fn is_screen_run(sel_method: &str, action: &str) -> bool {
    sel_method == "screen" && !is_preset(action)
}
fn timeframe_years(tf: &str) -> u32 {
    match tf { "1y" => 1, "3y" => 3, "5y" => 5, "10y" => 10, _ => 0 }
}

/// US long-term capital-gains rate for a taxable income (2025 single filer).
/// Display-only — keep the thresholds in sync with `US_LT_BRACKETS` in core.
fn us_lt_bracket(income: f64) -> f64 {
    if income <= 48_350.0 { 0.0 } else if income <= 533_400.0 { 0.15 } else { 0.20 }
}

// Shared knob-panel styles (mirror KnobGrid / MiniLabel in TaxSim.jsx).
const KNOB_GRID: &str = "display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:16px;padding:16px;background:var(--surface-sunken);border:2px solid var(--ink-800);border-radius:var(--radius-md);";
const MINI_LABEL: &str = "display:block;font-weight:600;font-size:12.5px;color:var(--text-strong);margin-bottom:7px;";
const RATE_NUM: &str = "font-family:var(--font-mono);font-variant-numeric:tabular-nums;font-weight:700;font-size:24px;letter-spacing:-0.02em;color:var(--accent);";
const RATE_CAP: &str = "display:block;font-size:10px;letter-spacing:0.08em;text-transform:uppercase;color:var(--text-muted);margin-top:2px;";

/// A muted note row prefixed with an info icon (TaxSim.jsx NoteLine).
fn note_line(text: &str) -> View {
    let t = text.to_string();
    view! {
        <div style="display:flex;gap:8px;align-items:flex-start;font-size:12px;color:var(--text-muted);line-height:1.45;">
            <span style="flex:none;margin-top:1px;color:var(--ink-300);"><Icon name="info".to_string() size=14 /></span>
            <span>{t}</span>
        </div>
    }.into_view()
}

/// Format a rate to ≤3 decimals, trimming trailing zeros ("26.375", "27.82").
fn round3(x: f64) -> String {
    format!("{:.3}", x).trim_end_matches('0').trim_end_matches('.').to_string()
}

/// US knobs: income + the long-term bracket chips that light from the income,
/// plus the NIIT chip. Mirrors `UsKnobs` in TaxSim.jsx.
fn us_knobs(income: RwSignal<f64>) -> View {
    view! {
        <div style=KNOB_GRID>
            <div>
                <span style=MINI_LABEL>"Annual taxable income"</span>
                <BdInput mono=true prefix="$".to_string()
                    value=format!("{:.0}", income.get_untracked())
                    on_input=Box::new(move |v| { if let Ok(n) = v.replace(',', "").parse::<f64>() { income.set(n.max(0.0)); } }) />
                <span style="display:block;margin-top:6px;font-size:11.5px;color:var(--text-muted);">"Sets which long-term bracket and the NIIT cliff apply."</span>
            </div>
            <div>
                <span style=MINI_LABEL>"Long-term rate \u{b7} NIIT"</span>
                {move || {
                    let inc = income.get();
                    let lt = us_lt_bracket(inc);
                    let chips = vec![
                        Chip { label: "0%".to_string(), on: lt == 0.0 },
                        Chip { label: "15%".to_string(), on: lt == 0.15 },
                        Chip { label: "20%".to_string(), on: lt == 0.20 },
                    ];
                    let niit = vec![Chip { label: "+3.8% NIIT".to_string(), on: inc > 200_000.0 }];
                    view! {
                        <div style="display:flex;flex-direction:column;gap:8px;">
                            <RateChips chips=chips />
                            <RateChips chips=niit />
                        </div>
                    }
                }}
            </div>
            <div style="grid-column:1 / -1;display:flex;flex-direction:column;gap:8px;">
                {note_line("Short- vs long-term holding split is worked out automatically from each trade's holding period.")}
                {note_line("Wash-sale rule is applied automatically \u{2014} losses on repurchases within 30 days are deferred, no knob needed.")}
            </div>
        </div>
    }.into_view()
}

/// German knobs: allowance + church/Vorabpauschale switches, the ETF Teilfreistellung
/// estimate, and an "Overall tax rate" callout (split when the estimate is on).
/// Mirrors `DeKnobs` in TaxSim.jsx.
fn de_knobs(
    allowance: RwSignal<f64>, church: RwSignal<bool>, vorab: RwSignal<bool>,
    estimate: RwSignal<bool>, teilfrei: RwSignal<f64>,
) -> View {
    view! {
        <div style="display:flex;flex-direction:column;gap:14px;">
            <div style=KNOB_GRID>
                <div>
                    <span style=MINI_LABEL>"Tax-free allowance"</span>
                    <BdInput mono=true prefix="\u{20ac}".to_string()
                        value=format!("{:.0}", allowance.get_untracked())
                        on_input=Box::new(move |v| { if let Ok(n) = v.replace(',', "").parse::<f64>() { allowance.set(n.max(0.0)); } }) />
                    <span style="display:block;margin-top:6px;font-size:11.5px;color:var(--text-muted);">"Sparerpauschbetrag \u{2014} exempt per year (\u{20ac}1,000 in 2025)."</span>
                </div>
                <div>
                    <span style=MINI_LABEL>"Church tax"</span>
                    <div style="display:flex;align-items:center;min-height:var(--control-md);">
                        {move || { let on = church.get(); view! { <BdSwitch checked=on on_change=Box::new(move |v| church.set(v)) label=(if on { "On" } else { "Off" }).to_string() /> } }}
                    </div>
                    <span style="display:block;margin-top:6px;font-size:11.5px;color:var(--text-muted);">"Adds Kirchensteuer (~+1.4 pts) on top of the base rate."</span>
                </div>
            </div>

            <div>
                <div style="display:flex;align-items:center;gap:7px;margin-bottom:8px;">
                    <span style="font-weight:700;font-size:10.5px;letter-spacing:0.1em;text-transform:uppercase;color:var(--accent);">"ETF rules \u{b7} applied to ETF holdings"</span>
                    <span style="flex:1;height:2px;background:var(--paper-300);" />
                </div>
                <div style=KNOB_GRID>
                    <div>
                        <span style=MINI_LABEL>"Teilfreistellung"</span>
                        {move || {
                            let on = estimate.get();
                            let dim = format!("flex:1;opacity:{};pointer-events:{};", if on { "1" } else { "0.45" }, if on { "auto" } else { "none" });
                            view! {
                                <div style="display:flex;align-items:center;gap:10px;">
                                    <BdSwitch checked=on on_change=Box::new(move |v| estimate.set(v)) />
                                    <div style=dim>
                                        <BdInput mono=true suffix="%".to_string()
                                            value=format!("{:.0}", teilfrei.get_untracked())
                                            on_input=Box::new(move |v| { if let Ok(n) = v.parse::<f64>() { teilfrei.set(n.clamp(0.0, 100.0)); } }) />
                                    </div>
                                </div>
                            }
                        }}
                        <span style="display:block;margin-top:6px;font-size:11.5px;color:var(--text-muted);">"Share of ETF gains exempt (equity ETFs: 30%)."</span>
                    </div>
                    <div>
                        <span style=MINI_LABEL>"Vorabpauschale"</span>
                        <div style="display:flex;align-items:center;min-height:var(--control-md);">
                            {move || { let on = vorab.get(); view! { <BdSwitch checked=on on_change=Box::new(move |v| vorab.set(v)) label=(if on { "On" } else { "Off" }).to_string() /> } }}
                        </div>
                        <span style="display:block;margin-top:6px;font-size:11.5px;color:var(--text-muted);">"Taxes a notional advance each year you hold."</span>
                    </div>
                </div>
                <div style="margin-top:10px;">
                    {note_line("Simplification: ETF rules apply to every ETF position; your direct stocks keep the full rate. Only equity ETFs (\u{2265}51% stocks) actually qualify for the 30% Teilfreistellung \u{2014} bond or low-equity funds get less, or none.")}
                </div>
            </div>

            {move || {
                let (ch, est, tf) = (church.get(), estimate.get(), teilfrei.get());
                let base = if ch { 27.82 } else { 26.375 };
                let etf = if est { base * (1.0 - tf / 100.0) } else { base };
                let breakdown = if est { "Direct holdings pay the full rate; ETF positions get Teilfreistellung relief." }
                    else if ch { "Abgeltungsteuer 25% + solidarity + church tax." }
                    else { "Abgeltungsteuer 25% + solidarity surcharge." };
                let rates = if est {
                    view! {
                        <div style="display:flex;gap:22px;align-items:flex-start;text-align:right;">
                            <div><span style=RATE_NUM>{format!("{}%", round3(base))}</span><span style=RATE_CAP>"Direct stocks"</span></div>
                            <div style="width:2px;align-self:stretch;background:var(--paper-300);" />
                            <div><span style=RATE_NUM>{format!("{}%", round3(etf))}</span><span style=RATE_CAP>"ETF holdings"</span></div>
                        </div>
                    }.into_view()
                } else {
                    view! { <span style="font-family:var(--font-mono);font-variant-numeric:tabular-nums;font-weight:700;letter-spacing:-0.02em;color:var(--accent);font-size:28px;">{format!("{}%", round3(base))}</span> }.into_view()
                };
                view! {
                    <div style="display:flex;align-items:center;justify-content:space-between;gap:16px;padding:14px 16px;min-height:88px;box-sizing:border-box;background:var(--surface-sunken);border:2px solid var(--ink-800);border-radius:var(--radius-md);">
                        <div style="flex:1;">
                            <span style="display:block;font-weight:700;font-size:11px;letter-spacing:0.1em;text-transform:uppercase;color:var(--text-strong);">"Overall tax rate"</span>
                            <span style="display:block;margin-top:3px;font-size:11.5px;color:var(--text-muted);">{breakdown}</span>
                        </div>
                        {rates}
                    </div>
                }
            }}
        </div>
    }.into_view()
}

/// Build the `&tax_*` query suffix for the backtest URL. Empty for "none";
/// otherwise carries the active system's knobs.
#[allow(clippy::too_many_arguments)]
fn tax_query(system: &str, income: f64, church: bool, allowance: f64, estimate: bool, teilfrei: f64, vorab: bool, sellall: bool) -> String {
    match system {
        "us" => format!("&tax=us&tax_income={income}&tax_sellall={sellall}"),
        "de" => format!(
            "&tax=de&tax_church={church}&tax_allowance={allowance}\
             &tax_estimate={estimate}&tax_teilfrei={teilfrei}&tax_vorab={vorab}&tax_sellall={sellall}"
        ),
        _ => String::new(),
    }
}

// ─── Formatting ───────────────────────────────────────────────────────────────

fn fmt_pct(x: f64) -> String {
    let v = x * 100.0;
    format!("{}{:.1}%", if v >= 0.0 { "+" } else { "\u{2212}" }, v.abs())
}
/// Ratios (Sharpe/Sortino/Recovery) — show "∞" for the no-downside / no-drawdown
/// case instead of "inf" or a misleading "0.00". (An infinite sortino is sent as
/// JSON null and deserializes to 0.0, so the infinite branch here is a guard.)
fn fmt_ratio(x: f64) -> String {
    if x.is_finite() { format!("{:.2}", x) } else { "\u{221e}".to_string() }
}

fn fmt_money(x: f64) -> String {
    if x.abs() >= 1_000_000.0 {
        format!("${:.2}M", x / 1_000_000.0)
    } else if x.abs() >= 1_000.0 {
        format!("${:.0}", x)
    } else {
        format!("${:.2}", x)
    }
}

// ─── API ──────────────────────────────────────────────────────────────────────

async fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    let resp = gloo_net::http::Request::get(url)
        .send().await.map_err(|e| e.to_string())?;
    if !resp.ok() { return Err(resp.text().await.unwrap_or_default()); }
    resp.json::<T>().await.map_err(|e| e.to_string())
}

// ─── Charts ───────────────────────────────────────────────────────────────────

fn svg_path(pts: &[(f64, f64)]) -> String {
    pts.iter().enumerate()
        .map(|(i, (x, y))| format!("{}{:.1},{:.1}", if i == 0 { "M" } else { " L" }, x, y))
        .collect()
}

/// Map a curve to chart points against explicit shared bounds, so two curves
/// (after-tax vs pre-tax) can be overlaid on one scale.
fn pts_with_bounds(curve: &[EquityPoint], dmin: i32, dmax: i32, ymin: f64, ymax: f64) -> Vec<(f64, f64)> {
    let dspan = (dmax - dmin).max(1) as f64;
    let yspan = (ymax - ymin).max(1e-9);
    curve.iter().map(|p| {
        let x = PAD + (p.date.num_days_from_ce() - dmin) as f64 / dspan * (W - PAD * 2.0);
        let y = PAD + (1.0 - (p.equity - ymin) / yspan) * (H - PAD * 2.0);
        (x, y)
    }).collect()
}

fn to_pts(result: &BacktestResult) -> Option<Vec<(f64, f64)>> {
    let curve = &result.curve;
    if curve.len() < 2 { return None; }
    let (mut dmin, mut dmax) = (i32::MAX, i32::MIN);
    let (mut ymin, mut ymax) = (f64::MAX, f64::MIN_POSITIVE);
    for p in curve {
        let d = p.date.num_days_from_ce();
        dmin = dmin.min(d); dmax = dmax.max(d);
        ymin = ymin.min(p.equity); ymax = ymax.max(p.equity);
    }
    let dspan = (dmax - dmin).max(1) as f64;
    let yspan = (ymax - ymin).max(1e-9);
    Some(curve.iter().map(|p| {
        let x = PAD + (p.date.num_days_from_ce() - dmin) as f64 / dspan * (W - PAD * 2.0);
        let y = PAD + (1.0 - (p.equity - ymin) / yspan) * (H - PAD * 2.0);
        (x, y)
    }).collect())
}

/// SVG equity curve with area gradient fill for a single result.
fn trade_timeline(trades: &[TradeEvent], dense: bool) -> View {
    if trades.is_empty() {
        return view! {
            <div style="text-align:center;padding:var(--space-6) var(--space-4);color:var(--text-muted);font-size:var(--text-sm);font-family:var(--font-mono);">
                "No trades executed. Bold of you."
            </div>
        }.into_view();
    }
    let row_gap = if dense { "var(--space-3)" } else { "var(--space-5)" };
    let marker = if dense { 30u32 } else { 36u32 };
    let marker_s = marker.to_string();
    let rows: Vec<_> = trades.iter().enumerate().map(|(i, t)| {
        let is_buy   = t.action == "buy";
        let is_first = i == 0;
        let is_last  = i == trades.len() - 1;
        let tone_color = if is_buy { "var(--gain)" } else { "var(--loss)" };
        let tone_soft  = if is_buy { "var(--gain-200)" } else { "var(--loss-200)" };
        let arrow      = if is_buy { "↑" } else { "↓" };
        let badge_label= if is_buy { "Buy" } else { "Sell" };
        let date_str   = format!("{}", t.date.format("%b %-d, %Y"));
        let price_str  = format!("${:.2}", t.price);
        let shares_str = format!("{:.1} sh", t.shares);
        let total_str  = format!("${:.2}", t.price * t.shares);
        let spine_top  = if is_first { format!("{}px", marker / 2) } else { "0".to_string() };
        let spine_bot  = if is_last  { format!("{}px", marker / 2) } else { "0".to_string() };
        let font_size  = if dense { "var(--text-sm)" } else { "var(--text-base)" };
        let row_pb     = if is_last { "0".to_string() } else { row_gap.to_string() };
        let marker_ss  = marker_s.clone();
        let col_style  = format!("position:relative;width:{marker_ss}px;flex:0 0 {marker_ss}px;display:flex;justify-content:center;");
        let spine_style= format!("position:absolute;top:{spine_top};bottom:{spine_bot};left:50%;width:2px;margin-left:-1px;background:var(--border-soft);");
        let dot_style  = format!("position:relative;z-index:1;width:{marker_ss}px;height:{marker_ss}px;flex:0 0 auto;border-radius:var(--radius-full);background:{tone_soft};border:var(--border-line) solid var(--ink-900);box-shadow:var(--shadow-hard-sm);display:flex;align-items:center;justify-content:center;font-family:var(--font-mono);font-weight:var(--weight-bold);font-size:17px;color:var(--ink-900);");
        let body_style = format!("flex:1;min-width:0;padding-bottom:{row_pb};padding-top:var(--space-1);");
        let tick_style = format!("font-family:var(--font-mono);font-weight:var(--weight-bold);font-size:{font_size};letter-spacing:0.01em;color:var(--text-strong);");
        let pill_style = format!("display:inline-flex;align-items:center;line-height:1;font-family:var(--font-body);font-weight:var(--weight-bold);font-size:var(--text-micro);letter-spacing:var(--tracking-overline);text-transform:uppercase;color:var(--paper-50);background:{tone_color};border:var(--border-hair) solid var(--ink-900);border-radius:var(--radius-full);padding:3px var(--space-2);");
        view! {
            <li style="display:flex;align-items:stretch;gap:var(--space-3);">
                <div style=col_style>
                    <span style=spine_style />
                    <span style=dot_style>{arrow}</span>
                </div>
                <div style=body_style>
                    <div style="display:flex;align-items:center;gap:var(--space-2);flex-wrap:wrap;">
                        <span style=tick_style>{t.ticker.clone()}</span>
                        <span style=pill_style>{badge_label}</span>
                        <span style="flex:1;" />
                        <span style="font-family:var(--font-mono);font-size:var(--text-xs);color:var(--text-muted);">{date_str}</span>
                    </div>
                    <div style="display:flex;align-items:baseline;gap:var(--space-2);flex-wrap:wrap;margin-top:var(--space-1);">
                        <span style="font-family:var(--font-mono);font-size:var(--text-sm);color:var(--text-body);">{price_str}</span>
                        <span style="font-family:var(--font-mono);font-size:var(--text-sm);color:var(--text-muted);">{"× "}{shares_str}</span>
                        <span style="flex:1;" />
                        <span style="font-family:var(--font-mono);font-weight:var(--weight-bold);font-size:var(--text-sm);color:var(--text-strong);">{total_str}</span>
                    </div>
                </div>
            </li>
        }
    }).collect();

    view! {
        <ol style="list-style:none;margin:0;padding:0;">
            {rows}
        </ol>
    }.into_view()
}

fn equity_single(r: &BacktestResult, label: &str) -> View {
    let Some(pts) = to_pts(r) else {
        return view! { <p style="color:var(--text-on-ink-muted);">"Not enough history to chart this one."</p> }.into_view();
    };
    // When a forward projection is attached, plot everything on one step-based
    // scale extended by the horizon, and build the p10–p90 fan. Otherwise keep the
    // date-based mapping (optionally with the pre-tax overlay). `projection_paths`
    // is `Some((band, p50, today_x))` only in the projection case.
    let (line, area, pretax_path, projection_paths) = if let Some(proj) = r.projection.as_ref().filter(|p| p.p50.len() >= 2) {
        let n = r.curve.len();
        let horizon = proj.p50.len() - 1;
        let total = ((n - 1) + horizon).max(1) as f64;
        let pre = r.pretax.as_ref().filter(|p| p.curve.len() == n);
        let (mut ymin, mut ymax) = (f64::MAX, f64::MIN_POSITIVE);
        for pt in &r.curve { ymin = ymin.min(pt.equity); ymax = ymax.max(pt.equity); }
        if let Some(p) = pre { for pt in &p.curve { ymin = ymin.min(pt.equity); ymax = ymax.max(pt.equity); } }
        for v in proj.p10.iter().chain(proj.p90.iter()) { ymin = ymin.min(*v); ymax = ymax.max(*v); }
        let yspan = (ymax - ymin).max(1e-9);
        let sx = |i: f64| PAD + (i / total) * (W - PAD * 2.0);
        let sy = |v: f64| PAD + (1.0 - (v - ymin) / yspan) * (H - PAD * 2.0);
        let line_of = |vals: &[f64], start: usize| -> String {
            vals.iter().enumerate()
                .map(|(j, &v)| format!("{}{:.1},{:.1}", if j == 0 { "M" } else { " L" }, sx((start + j) as f64), sy(v)))
                .collect()
        };
        let curve_eq: Vec<f64> = r.curve.iter().map(|p| p.equity).collect();
        let l = line_of(&curve_eq, 0);
        let anchor_x = sx((n - 1) as f64);
        let h_bot = H - PAD;
        let a = format!("{l} L{:.1},{:.1} L{:.1},{:.1} Z", anchor_x, h_bot, PAD, h_bot);
        let pre_path = pre.map(|p| line_of(&p.curve.iter().map(|x| x.equity).collect::<Vec<_>>(), 0));
        // Fan band: p90 forward (anchored at step n-1) then p10 reversed.
        let p90_fwd: String = proj.p90.iter().enumerate()
            .map(|(j, &v)| format!("{}{:.1},{:.1}", if j == 0 { "M" } else { " L" }, sx((n - 1 + j) as f64), sy(v))).collect();
        let p10_rev: String = (0..proj.p10.len()).rev()
            .map(|j| format!(" L{:.1},{:.1}", sx((n - 1 + j) as f64), sy(proj.p10[j]))).collect();
        let band = format!("{p90_fwd}{p10_rev} Z");
        let p50 = line_of(&proj.p50, n - 1);
        (l, a, pre_path, Some((band, p50, anchor_x)))
    } else {
        let pre = r.pretax.as_ref().filter(|p| p.curve.len() >= 2);
        let (after_pts, pre_path) = if let Some(p) = pre {
            let (mut dmin, mut dmax) = (i32::MAX, i32::MIN);
            let (mut ymin, mut ymax) = (f64::MAX, f64::MIN_POSITIVE);
            for c in [&r.curve, &p.curve] {
                for pt in c.iter() {
                    let d = pt.date.num_days_from_ce();
                    dmin = dmin.min(d); dmax = dmax.max(d);
                    ymin = ymin.min(pt.equity); ymax = ymax.max(pt.equity);
                }
            }
            let after = pts_with_bounds(&r.curve, dmin, dmax, ymin, ymax);
            let prep = svg_path(&pts_with_bounds(&p.curve, dmin, dmax, ymin, ymax));
            (after, Some(prep))
        } else {
            (pts.clone(), None)
        };
        let l = svg_path(&after_pts);
        let (fx, _) = after_pts[0];
        let (lx, _) = *after_pts.last().unwrap();
        let h_bot = format!("{:.1}", H - PAD);
        let a = format!("{} L{:.1},{h_bot} L{:.1},{h_bot} Z", l, lx, fx);
        (l, a, pre_path, None)
    };

    // Projection fan pieces (only Some when a projection is attached).
    let proj_band  = projection_paths.as_ref().map(|(b, _, _)| b.clone());
    let proj_p50   = projection_paths.as_ref().map(|(_, p, _)| p.clone());
    let proj_today = projection_paths.as_ref().map(|(_, _, t)| format!("{:.1}", t));
    let has_projection = projection_paths.is_some();
    let sep_y1 = format!("{:.0}", PAD);
    let sep_y2 = format!("{:.1}", H - PAD);

    let win          = r.metrics.total_return >= 0.0;
    let color        = if win { "var(--gain-200)" } else { "var(--loss-200)" };
    let total_ret    = fmt_pct(r.metrics.total_return);
    let total_ret_s  = total_ret.clone(); // for BdStat (badge below owns original)
    let badge_tone   = if win { "gain" } else { "loss" };
    let card_title   = if win { "You'd have made money" } else { "You'd have lost money" }.to_string();
    let init_str     = fmt_money(r.initial_amount);
    let card_ol      = format!("{label} · starts at {init_str}");
    let final_str    = fmt_money(r.final_value);
    let cagr_str     = format!("{} /yr", fmt_pct(r.metrics.cagr));
    let mdd_str      = fmt_pct(r.metrics.max_drawdown);
    let sharpe_str   = fmt_ratio(r.metrics.sharpe);
    let sortino_str  = fmt_ratio(r.metrics.sortino);
    // No drawdown → recovery factor is undefined (core returns 0.0); show ∞, not "0.00".
    let recovery_str = if r.metrics.max_drawdown >= 0.0 { "\u{221e}".to_string() }
                       else { fmt_ratio(r.metrics.recovery_factor) };
    let bag          = r.metrics.max_drawdown < -0.30;
    let opp_pct      = (r.metrics.max_drawdown.abs() * 100.0).round() as i64;
    let mdd_bag      = fmt_pct(r.metrics.max_drawdown);

    let gy1 = format!("{:.1}", PAD + (H - PAD * 2.0) * 0.25);
    let gy2 = format!("{:.1}", PAD + (H - PAD * 2.0) * 0.50);
    let gy3 = format!("{:.1}", PAD + (H - PAD * 2.0) * 0.75);
    let x1s = format!("{PAD}");
    let x2s = format!("{:.1}", W - PAD);
    let vb   = format!("0 0 {W} {H}");
    let hs   = format!("{H}");
    let sw   = format!("width:16px;height:3px;background:{color};border-radius:2px;");

    let bench_view = r.benchmark.as_ref().map(|b| {
        let b_cagr     = format!("{} /yr", fmt_pct(b.metrics.cagr));
        let b_mdd      = fmt_pct(b.metrics.max_drawdown);
        let b_sharpe   = fmt_ratio(b.metrics.sharpe);
        let b_sortino  = fmt_ratio(b.metrics.sortino);
        let b_recovery = if b.metrics.max_drawdown >= 0.0 { "\u{221e}".to_string() }
                         else { fmt_ratio(b.metrics.recovery_factor) };
        let b_ret    = fmt_pct(b.metrics.total_return);
        let b_final  = fmt_money(b.final_value);
        let b_tone   = if b.metrics.total_return >= 0.0 { "gain" } else { "loss" };
        // The benchmark stays pre-tax even when the strategy is taxed — flag it so
        // the comparison isn't misread as the strategy underperforming.
        let bench_pretax_note = (r.tax_system != TaxSystem::None).then(|| view! {
            <p style="margin:0 0 -4px;font-family:var(--font-mono);font-size:11px;color:var(--text-faint);">
                "Benchmark shown pre-tax."
            </p>
        });
        view! {
            <>
            {bench_pretax_note}
            <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(175px,1fr));gap:var(--space-3);">
                <BdCard padding="16px".to_string()>
                    <BdStat label="Bench. value".to_string() value=b_final size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Bench. return".to_string() value=b_ret size="sm".to_string()
                        delta_tone=b_tone.to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Bench. CAGR".to_string() value=b_cagr size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Bench. MDD".to_string() value=b_mdd size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Bench. Sharpe".to_string() value=b_sharpe size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Bench. Sortino".to_string() value=b_sortino size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Bench. Recovery".to_string() value=b_recovery size="sm".to_string() />
                </BdCard>
            </div>
            </>
        }
    });

    let has_trades = r.trades.len() > 1;
    let trade_count = r.trades.len();
    let trade_title = format!("{} {}", trade_count, if trade_count == 1 { "fill" } else { "fills" });
    let trade_ticker = r.trades.first().map(|t| t.ticker.clone()).unwrap_or_default();
    let trades_dense = trade_count > 5;
    let trades_view = if has_trades { trade_timeline(&r.trades, trades_dense) } else { view! {}.into_view() };
    let equity_col  = if has_trades { "minmax(0,1.65fr)" } else { "1fr" };
    let row_style   = format!("display:grid;grid-template-columns:{equity_col}{};gap:18px;align-items:start;",
                              if has_trades { " minmax(300px,1fr)" } else { "" });

    // ── Tax: after-tax-vs-pre-tax pairing + per-year drag (F6) ─────────────
    let tax_active = r.tax_system != TaxSystem::None;
    let sys_label = match r.tax_system {
        TaxSystem::UsFederal => "U.S. federal",
        TaxSystem::Germany => "German",
        TaxSystem::None => "",
    };
    let tax_view = tax_active.then(|| {
        let after_final = fmt_money(r.final_value);
        let after_cagr  = format!("{} /yr", fmt_pct(r.metrics.cagr));
        let total_tax_s = fmt_money(r.total_tax);
        let (pre_final, pre_cagr) = r.pretax.as_ref()
            .map(|p| (fmt_money(p.final_value), format!("{} /yr", fmt_pct(p.metrics.cagr))))
            .unwrap_or_else(|| ("—".to_string(), "—".to_string()));
        let eff = r.pretax.as_ref().map(|p| {
            let g = p.final_value - r.initial_amount;
            if g > 0.0 { r.total_tax / g } else { 0.0 }
        }).unwrap_or(0.0);
        let eff_s = format!("{:.1}%", eff * 100.0);

        // Paired after-tax / pre-tax stat. After-tax is the headline; the pre-tax
        // twin sits below struck through.
        let paired = |label: &str, after: String, pre: String| {
            let l = label.to_string();
            view! {
                <BdCard padding="16px".to_string()>
                    <div style="display:flex;flex-direction:column;gap:5px;">
                        <span style="font-weight:700;font-size:var(--text-micro);letter-spacing:var(--tracking-overline);text-transform:uppercase;color:var(--text-muted);">{l}</span>
                        <span style="font-family:var(--font-mono);font-variant-numeric:tabular-nums;font-weight:700;font-size:var(--text-title);line-height:1;color:var(--text-strong);">{after}</span>
                        <span style="font-family:var(--font-mono);font-size:11.5px;color:var(--text-faint);text-decoration:line-through;">{pre}</span>
                    </div>
                </BdCard>
            }
        };

        // Per-year tax drag.
        let max_tax = r.tax_per_year.iter().map(|y| y.tax).fold(1.0_f64, f64::max);
        let drag_rows: Vec<_> = r.tax_per_year.iter().map(|y| {
            let w = format!("{:.0}%", (y.tax / max_tax * 100.0).clamp(0.0, 100.0));
            let bar_style = format!("height:10px;background:var(--loss);border-radius:2px;width:{w};");
            let yr = y.year.to_string();
            let gain_s = format!("{} gain", fmt_money(y.gain));
            let tax_s = if y.tax > 0.0 { fmt_money(-y.tax) } else { "$0".to_string() };
            let tax_color = if y.tax > 0.0 { "var(--loss)" } else { "var(--text-faint)" };
            let tax_style = format!("font-family:var(--font-mono);font-weight:700;font-size:13px;text-align:right;color:{tax_color};");
            view! {
                <div style="display:grid;grid-template-columns:48px 1fr 92px;align-items:center;gap:12px;padding:7px 4px;border-bottom:1px solid var(--border-soft);">
                    <span style="font-family:var(--font-mono);font-weight:700;font-size:12.5px;color:var(--text-strong);">{yr}</span>
                    <div style="display:flex;align-items:center;gap:10px;">
                        <div style="flex:1;height:10px;background:var(--surface-sunken);border:1px solid var(--border-soft);border-radius:3px;overflow:hidden;">
                            <div style=bar_style />
                        </div>
                        <span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted);white-space:nowrap;">{gain_s}</span>
                    </div>
                    <span style=tax_style>{tax_s}</span>
                </div>
            }
        }).collect();
        let drag_panel = (!r.tax_per_year.is_empty()).then(|| view! {
            <BdCard overline="Per-year tax drag".to_string() title="What the taxman took, by year".to_string()>
                <div style="margin-top:8px;max-height:268px;overflow-y:auto;">{drag_rows}</div>
                <div style="display:flex;justify-content:space-between;align-items:center;margin-top:12px;padding-top:10px;border-top:var(--border-line) solid var(--ink-900);">
                    <span style="font-size:12px;color:var(--text-muted);">"Effective tax on gains"</span>
                    <span style="font-family:var(--font-mono);font-weight:700;font-size:14px;color:var(--text-strong);">{eff_s}</span>
                </div>
            </BdCard>
        });

        view! {
            <div style="display:flex;flex-direction:column;gap:var(--space-3);padding:16px;background:var(--surface-sunken);border:var(--border-line) solid var(--ink-900);border-radius:var(--radius-lg);">
                <span style="font-weight:700;font-size:var(--text-micro);letter-spacing:var(--tracking-overline);text-transform:uppercase;color:var(--accent);">
                    {format!("{sys_label} tax · what you actually keep")}
                </span>
                <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(175px,1fr));gap:var(--space-3);">
                    {paired("After-tax value", after_final, pre_final)}
                    {paired("After-tax CAGR", after_cagr, pre_cagr)}
                    <BdCard padding="16px".to_string()>
                        <BdStat label="Total tax paid".to_string() value=total_tax_s size="sm".to_string() delta_tone="loss".to_string() />
                    </BdCard>
                </div>
                {drag_panel}
            </div>
        }
    });

    let disclaimer = if tax_active {
        format!("After {sys_label} tax. Excludes slippage and survivorship bias. Past performance is a vibe, not a promise.")
    } else {
        "Excludes taxes, slippage, and survivorship bias. Past performance is a vibe, not a promise.".to_string()
    };

    view! {
        <div style="display:flex;flex-direction:column;gap:var(--space-4);">
            {bench_view}
            <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(175px,1fr));gap:var(--space-3);">
                <BdCard padding="16px".to_string()>
                    <BdStat label="Final value".to_string() value=final_str size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Total return".to_string() value=total_ret_s size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="CAGR".to_string() value=cagr_str size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Max drawdown".to_string() value=mdd_str size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Sharpe ratio".to_string() value=sharpe_str size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Sortino ratio".to_string() value=sortino_str size="sm".to_string() />
                </BdCard>
                <BdCard padding="16px".to_string()>
                    <BdStat label="Recovery factor".to_string() value=recovery_str size="sm".to_string() />
                </BdCard>
            </div>

            {tax_view}

            <div style=row_style>
            <BdCard tone="dark".to_string() overline=card_ol title=card_title>
                <div style="position:absolute;top:16px;right:16px;">
                    <BdBadge tone=badge_tone.to_string()>{total_ret}</BdBadge>
                </div>
                <div style="margin-top:6px;">
                    <svg viewBox=vb width="100%" height=hs preserveAspectRatio="none"
                         style="display:block;overflow:visible;">
                        <defs>
                            <linearGradient id="bd_eq_grad" x1="0" y1="0" x2="0" y2="1">
                                <stop offset="0%"   stop-color=color stop-opacity="0.25" />
                                <stop offset="100%" stop-color=color stop-opacity="0.03" />
                            </linearGradient>
                        </defs>
                        <line x1=x1s.clone() x2=x2s.clone() y1=gy1.clone() y2=gy1
                              stroke="var(--grid-on-dark)" stroke-width="1" stroke-dasharray="3 5" />
                        <line x1=x1s.clone() x2=x2s.clone() y1=gy2.clone() y2=gy2
                              stroke="var(--grid-on-dark)" stroke-width="1" stroke-dasharray="3 5" />
                        <line x1=x1s x2=x2s y1=gy3.clone() y2=gy3
                              stroke="var(--grid-on-dark)" stroke-width="1" stroke-dasharray="3 5" />
                        <path d=area fill="url(#bd_eq_grad)" />
                        {proj_band.clone().map(|d| view! {
                            <path d=d fill="var(--paper-50)" fill-opacity="0.16" stroke="none" />
                        })}
                        {proj_today.clone().map(|x| view! {
                            <line x1=x.clone() x2=x y1=sep_y1.clone() y2=sep_y2.clone()
                                  stroke="var(--paper-50)" stroke-opacity="0.55" stroke-width="1.5" stroke-dasharray="2 4" />
                        })}
                        {pretax_path.clone().map(|d| view! {
                            <path d=d fill="none" stroke="var(--text-on-ink-muted)" stroke-width="1.5"
                                  stroke-dasharray="4 4" stroke-linejoin="round" opacity="0.7" />
                        })}
                        <path d=line fill="none" stroke=color stroke-width="2.5"
                              stroke-linejoin="round" stroke-linecap="round" />
                        {proj_p50.clone().map(|d| view! {
                            <path d=d fill="none" stroke="var(--paper-50)" stroke-opacity="0.85" stroke-width="2"
                                  stroke-dasharray="6 5" stroke-linejoin="round" stroke-linecap="round" />
                        })}
                    </svg>
                    <div style="display:flex;gap:18px;margin-top:var(--space-3);font-size:var(--text-xs);\
                                color:var(--text-on-ink-muted);font-family:var(--font-mono);">
                        <span style="display:inline-flex;align-items:center;gap:7px;">
                            <span style=sw />{if pretax_path.is_some() { "After tax" } else { "Strategy" }}
                        </span>
                        {pretax_path.is_some().then(|| view! {
                            <span style="display:inline-flex;align-items:center;gap:7px;">
                                <span style="width:16px;height:0;border-top:2px dashed var(--text-on-ink-muted);" />"Pre-tax"
                            </span>
                        })}
                        {has_projection.then(|| view! {
                            <span style="display:inline-flex;align-items:center;gap:7px;">
                                <span style="width:16px;height:0;border-top:2px dashed var(--paper-50);opacity:0.85;" />"Projection p10/p50/p90"
                            </span>
                        })}
                    </div>
                    {has_projection.then(|| view! {
                        <p style="margin:var(--space-2) 0 0;font-family:var(--font-mono);font-size:11px;color:var(--text-on-ink-muted);">
                            "1000 bootstrap paths · p10/p50/p90 · horizon = backtest length"
                        </p>
                    })}
                </div>
            </BdCard>

            {has_trades.then(|| view! {
                <BdCard overline="Executed trades".to_string() title=trade_title>
                    <div style="position:absolute;top:16px;right:16px;">
                        <BdBadge tone="neutral".to_string() soft=true>{trade_ticker}</BdBadge>
                    </div>
                    <div style="max-height:318px;overflow-y:auto;margin:0 -4px;padding:2px var(--space-1);">
                        {trades_view}
                    </div>
                </BdCard>
            })}
            </div>

            {bag.then(|| view! {
                <div style="display:flex;gap:14px;align-items:flex-start;padding:18px 20px;\
                            background:var(--loss-200);border:var(--border-bold) solid var(--ink-900);\
                            border-radius:var(--radius-lg);box-shadow:var(--shadow-hard);">
                    <span style="flex:none;width:44px;height:44px;border-radius:50%;\
                                 background:var(--loss);border:var(--border-line) solid var(--ink-900);\
                                 display:flex;align-items:center;justify-content:center;\
                                 font-size:var(--text-title);line-height:1;">"🛍"</span>
                    <div>
                        <div style="font-family:var(--font-display);font-weight:800;font-size:19px;\
                                    letter-spacing:-0.01em;color:var(--loss-600);margin-bottom:3px;">
                            "Congratulations, you're a bagholder."
                        </div>
                        <p style="margin:0;font-size:var(--text-sm);line-height:1.5;color:var(--text-body);">
                            "This position fell " {mdd_bag}
                            " with no stop loss. If you'd just held cash, you'd be about "
                            <strong>{format!("{opp_pct}% richer")}</strong>
                            " right now. The DeLorean can't fix conviction."
                        </p>
                    </div>
                </div>
            })}

            <p style="font-family:var(--font-mono);font-size:11.5px;\
                      color:var(--text-faint);margin:2px 0 0;text-align:center;">
                {disclaimer}
            </p>
        </div>
    }
    .into_view()
}

/// Multi-series overlay chart for screen × action results.
fn equity_overlay(series: &[(String, BacktestResult)]) -> View {
    let series: Vec<&(String, BacktestResult)> =
        series.iter().filter(|(_, r)| r.curve.len() >= 2).collect();
    if series.is_empty() {
        return view! { <p style="color:var(--text-on-ink-muted);">"Not enough history to chart this one."</p> }.into_view();
    }
    let (mut dmin, mut dmax) = (i32::MAX, i32::MIN);
    let (mut ymin, mut ymax) = (f64::MAX, f64::MIN_POSITIVE);
    for (_, r) in &series {
        for p in &r.curve { let d = p.date.num_days_from_ce(); dmin = dmin.min(d); dmax = dmax.max(d); ymin = ymin.min(p.equity); ymax = ymax.max(p.equity); }
    }
    let dspan = (dmax - dmin).max(1) as f64;
    let yspan = (ymax - ymin).max(1e-9);

    let paths_meta: Vec<(String, &str, String, f64, Option<NaiveDate>)> = series.iter().enumerate().map(|(i, (name, r))| {
        let color = OVERLAY_COLORS[i % OVERLAY_COLORS.len()];
        let pts: Vec<(f64, f64)> = r.curve.iter().map(|p| {
            (PAD + (p.date.num_days_from_ce() - dmin) as f64 / dspan * (W - PAD * 2.0),
             PAD + (1.0 - (p.equity - ymin) / yspan) * (H - PAD * 2.0))
        }).collect();
        let ret = r.curve.last().map(|p| p.equity / r.curve[0].equity - 1.0).unwrap_or(0.0);
        (svg_path(&pts), color, name.clone(), ret, r.entry_date)
    }).collect();

    let legend = paths_meta.iter().map(|(_, color, name, ret, entry)| {
        let rs   = fmt_pct(*ret);
        let rc   = if *ret >= 0.0 { "var(--gain-200)" } else { "var(--loss-200)" };
        let sw   = format!("width:16px;height:3px;background:{color};border-radius:2px;");
        let from = entry.map(|d| format!("from {d}")).unwrap_or_default();
        view! {
            <span style="display:inline-flex;align-items:center;gap:var(--space-2);\
                         font-family:var(--font-mono);font-size:13px;">
                <span style=sw />
                <span style="color:var(--paper-50);font-weight:700;">{name.clone()}</span>
                {(!from.is_empty()).then(|| view! { <span style="color:var(--text-muted);">{from}</span> })}
                <span style=format!("color:{rc};")>{rs}</span>
            </span>
        }
    }).collect_view();

    let lines = paths_meta.iter().map(|(d, color, _, _, _)| view! {
        <path d=d.clone() fill="none" stroke=*color stroke-width="2.5"
              stroke-linejoin="round" stroke-linecap="round" />
    }).collect_view();

    let gy1 = format!("{:.1}", PAD + (H - PAD * 2.0) * 0.25);
    let gy2 = format!("{:.1}", PAD + (H - PAD * 2.0) * 0.50);
    let gy3 = format!("{:.1}", PAD + (H - PAD * 2.0) * 0.75);
    let x1s = format!("{PAD}"); let x2s = format!("{:.1}", W - PAD);
    let vb  = format!("0 0 {W} {H}"); let hs = format!("{H}");

    view! {
        <div>
            <svg viewBox=vb width="100%" height=hs preserveAspectRatio="none"
                 style="display:block;overflow:visible;">
                <line x1=x1s.clone() x2=x2s.clone() y1=gy1.clone() y2=gy1 stroke="var(--grid-on-dark)" stroke-width="1" stroke-dasharray="3 5" />
                <line x1=x1s.clone() x2=x2s.clone() y1=gy2.clone() y2=gy2 stroke="var(--grid-on-dark)" stroke-width="1" stroke-dasharray="3 5" />
                <line x1=x1s x2=x2s y1=gy3.clone() y2=gy3 stroke="var(--grid-on-dark)" stroke-width="1" stroke-dasharray="3 5" />
                {lines}
            </svg>
            <div style="display:flex;flex-wrap:wrap;gap:var(--space-4);margin-top:14px;">{legend}</div>
        </div>
    }.into_view()
}

/// Per-ticker P/E mini-chart with trough dots.
fn pe_chart(ticker: &str, h: &PeHistory, entry: Option<NaiveDate>) -> View {
    if h.series.len() < 2 {
        return view! { <p style="font-size:.85rem;color:var(--text-muted);">{ticker.to_string()}": no P/E history"</p> }.into_view();
    }
    let (cw, ch) = (280.0_f64, 62.0_f64);
    let (mut dmin, mut dmax) = (i32::MAX, i32::MIN);
    let (mut pmin, mut pmax) = (f64::MAX, f64::MIN_POSITIVE);
    for p in &h.series { let d = p.date.num_days_from_ce(); dmin = dmin.min(d); dmax = dmax.max(d); pmin = pmin.min(p.pe); pmax = pmax.max(p.pe); }
    let dspan = (dmax - dmin).max(1) as f64;
    let pspan = (pmax - pmin).max(1e-9);
    let xy = |date: NaiveDate, pe: f64| -> (f64, f64) {
        (6.0 + (date.num_days_from_ce() - dmin) as f64 / dspan * (cw - 12.0),
         ch - 6.0 - (pe - pmin) / pspan * (ch - 12.0))
    };
    let line: String = h.series.iter().enumerate().map(|(i, p)| {
        let (x, y) = xy(p.date, p.pe);
        format!("{}{:.1},{:.1}", if i == 0 { "M" } else { " L" }, x, y)
    }).collect();
    let dots = h.troughs.iter().map(|t| {
        let (x, y) = xy(t.date, t.pe);
        let act    = entry == Some(t.date);
        view! {
            <circle cx=format!("{x:.1}") cy=format!("{y:.1}") r=if act {"5"} else {"3.5"}
                    fill=if act {"var(--loss)"} else {"var(--paper-50)"}
                    stroke=if act {"var(--ink-900)"} else {"var(--ink-500)"}
                    stroke-width="2" />
        }
    }).collect_view();
    let vb = format!("0 0 {cw} {ch}");
    let hs = format!("{ch}");
    let lg = format!("P/E {pmin:.1}–{pmax:.1}");
    view! {
        <div style="background:var(--teal-600);border:var(--border-line) solid var(--ink-900);\
                    border-radius:var(--radius-md);padding:var(--space-3) 14px;">
            <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:6px;">
                <span style="font-family:var(--font-mono);font-weight:700;font-size:13px;color:var(--paper-50);">
                    {ticker.to_string()}
                </span>
                <span style="font-size:var(--text-micro);color:var(--text-on-ink-muted);">{lg}</span>
            </div>
            <svg viewBox=vb width="100%" height=hs preserveAspectRatio="none" style="display:block;">
                <path d=line fill="none" stroke="var(--ink-500)" stroke-width="2" stroke-linejoin="round" />
                {dots}
            </svg>
        </div>
    }.into_view()
}

// ─── ConcernPanel ─────────────────────────────────────────────────────────────

#[component]
fn ConcernPanel(
    #[prop(into)] step:  String,
    #[prop(into)] title: String,
    #[prop(into, optional)] question: Option<String>,
    #[prop(default = false)] disabled: bool,
    children: Children,
) -> impl IntoView {
    let inner = format!(
        "display:flex;flex-direction:column;padding:var(--space-4);\
         background:var(--surface-sunken);border:var(--border-line) solid var(--ink-800);\
         border-radius:var(--radius-md);min-height:104px;justify-content:center;{}",
        if disabled { "opacity:0.4;pointer-events:none;filter:saturate(0.5);" } else { "" }
    );
    view! {
        <div style="display:flex;flex-direction:column;gap:9px;">
            <div style="display:flex;flex-direction:column;gap:2px;padding-left:2px;">
                <div style="display:flex;align-items:baseline;gap:7px;">
                    <span style="font-family:var(--font-mono);font-weight:700;font-size:var(--text-micro);\
                                 color:var(--accent);">{step}</span>
                    <span style="font-weight:700;font-size:var(--text-micro);letter-spacing:0.1em;\
                                 text-transform:uppercase;color:var(--text-strong);">{title}</span>
                </div>
                {question.map(|q| view! {
                    <span style="font-size:11.5px;color:var(--text-muted);font-style:italic;">{q}</span>
                })}
            </div>
            <div style=inner>{children()}</div>
        </div>
    }
}

/// Index + uppercase title (+ optional italic question) — the heading that sits
/// above each config field, matching the ConcernPanel headings.
fn field_heading(step: Option<&str>, title: &str, question: Option<&str>) -> View {
    let title = title.to_string();
    let step  = step.map(str::to_string);
    let question = question.map(str::to_string);
    view! {
        <div style="display:flex;flex-direction:column;gap:2px;padding-left:2px;">
            <div style="display:flex;align-items:baseline;gap:7px;">
                {step.map(|s| view! {
                    <span style="font-family:var(--font-mono);font-weight:700;font-size:var(--text-micro);\
                                 color:var(--accent);">{s}</span>
                })}
                <span style="font-weight:700;font-size:var(--text-micro);letter-spacing:0.1em;\
                             text-transform:uppercase;color:var(--text-strong);white-space:nowrap;">
                    {title}
                </span>
            </div>
            {question.map(|q| view! {
                <span style="font-size:11.5px;color:var(--text-muted);font-style:italic;">{q}</span>
            })}
        </div>
    }.into_view()
}

/// Smooth-scroll a stacked section into view by id. `html { scroll-behavior:
/// smooth }` (ds.css) animates it; Run jumps to Simulation, Back to Config.
fn scroll_to(id: &str) {
    if let Some(el) = document().get_element_by_id(id) {
        el.scroll_into_view();
    }
}

// ─── App ──────────────────────────────────────────────────────────────────────

#[component]
fn App() -> impl IntoView {
    // ── Signals ───────────────────────────────────────────────────────────────
    let action      = create_rw_signal("buyhold".to_string());
    let sel_method  = create_rw_signal("ticker".to_string());
    let screen_kind = create_rw_signal("lowpe".to_string());
    let ticker      = create_rw_signal("AAPL".to_string());
    let timeframe   = create_rw_signal("10y".to_string());
    let fast          = create_rw_signal(20usize);
    let slow          = create_rw_signal(50usize);
    let rsi_threshold = create_rw_signal(20.0f64);
    let ticker_a      = create_rw_signal("KO".to_string());
    let ticker_b      = create_rw_signal("PEP".to_string());
    let entry_z       = create_rw_signal(2.0f64);
    let top_n         = create_rw_signal(3usize);
    let realistic     = create_rw_signal(false);
    let initial_amount     = create_rw_signal(10_000.0f64);
    let benchmark_ticker   = create_rw_signal("SPY".to_string());
    let benchmark_strategy = create_rw_signal("buy_and_hold".to_string());
    let show_benchmark     = create_rw_signal(false);
    // Tax configurator state (F6). system: "none" | "us" | "de".
    let tax_enabled    = create_rw_signal(false);
    let tax_collapsed  = create_rw_signal(false);
    let tax_system     = create_rw_signal("none".to_string());
    let tax_income     = create_rw_signal(96_000.0f64);
    let tax_church     = create_rw_signal(false);
    let tax_allowance  = create_rw_signal(1_000.0f64);
    let tax_estimate   = create_rw_signal(false);
    let tax_teilfrei   = create_rw_signal(30.0f64);
    let tax_vorab      = create_rw_signal(true);
    let tax_sellall    = create_rw_signal(true);
    let project_fwd    = create_rw_signal(false);

    // Fetch universe once on mount for datalist autocomplete.
    let universe = create_resource(
        || (),
        |_| async { get_json::<Vec<String>>("/api/universe").await.unwrap_or_default() },
    );

    let busy          = create_rw_signal(false);
    let single_result = create_rw_signal::<Option<Result<BacktestResult, String>>>(None);
    let candidates    = create_rw_signal::<Option<Result<Vec<Candidate>, String>>>(None);
    let selected      = create_rw_signal::<HashSet<String>>(HashSet::new());
    let overlay       = create_rw_signal::<Vec<(String, BacktestResult)>>(Vec::new());
    let pe_entry      = create_rw_signal(false);
    let pe_index      = create_rw_signal(0usize);
    let pe_hist       = create_rw_signal::<HashMap<String, PeHistory>>(HashMap::new());
    // ── Handlers — all captures are RwSignal (Copy+'static) ──────────────────
    let run = move || {
        let a    = action.get();
        let prst = is_preset(&a);
        let use_screen = is_screen_run(&sel_method.get(), &a);
        single_result.set(None);

        if prst {
            if is_meme(&a) {
                single_result.set(Some(Err(format!(
                    "{} needs an external data source not yet available.", action_label(&a)
                ))));
                return;
            }
            let url = match a.as_str() {
                "pairs"      => format!("/api/preset?kind=pairs&ticker_a={}&ticker_b={}&entry_z={}",
                    ticker_a.get(), ticker_b.get(), entry_z.get()),
                "riskparity" => "/api/preset?kind=risk_parity".to_string(),
                "sectorrot"  => "/api/preset?kind=sector_rotation".to_string(),
                _            => "/api/preset?kind=econ_cycle".to_string(),
            };
            busy.set(true); candidates.set(None);
            spawn_local(async move {
                single_result.set(Some(get_json::<BacktestResult>(&url).await));
                busy.set(false);
            });
        } else if use_screen {
            let sk = screen_kind.get();
            busy.set(true); candidates.set(None);
            selected.update(|s| s.clear());
            overlay.set(Vec::new()); pe_hist.update(|m| m.clear());
            spawn_local(async move {
                let r = if sk != "lowpe" {
                    Err("This screen isn't implemented yet — only Low P/E is available.".to_string())
                } else {
                    get_json::<Vec<Candidate>>("/api/screen?kind=low_pe&limit=12").await
                };
                candidates.set(Some(r)); busy.set(false);
            });
        } else {
            let t = ticker.get();
            let a = action.get();
            let years = timeframe_years(&timeframe.get());
            let amt = initial_amount.get();
            let bench_suffix = if show_benchmark.get() {
                let bt = benchmark_ticker.get();
                let bs = benchmark_strategy.get();
                format!("&benchmark_ticker={bt}&benchmark_strategy={bs}")
            } else { String::new() };
            let tax_suffix = tax_query(
                &tax_system.get(), tax_income.get(), tax_church.get(), tax_allowance.get(),
                tax_estimate.get(), tax_teilfrei.get(), tax_vorab.get(), tax_sellall.get(),
            );
            let project_suffix = if project_fwd.get() { "&project=true" } else { "" };
            let bench_suffix = format!("{bench_suffix}{tax_suffix}{project_suffix}");
            let url = if a == "congress" {
                format!(
                    "/api/backtest?ticker={t}&strategy=congress_copy_trade&year=2023\
                     &use_filing_date={}&years={years}&initial_amount={amt}{bench_suffix}",
                    realistic.get()
                )
            } else if a == "cramer" {
                format!("/api/backtest?ticker={t}&strategy=cramer_inverse&years={years}&initial_amount={amt}{bench_suffix}")
            } else if a == "short_squeeze" {
                format!("/api/backtest?ticker={t}&strategy=short_squeeze&years={years}&initial_amount={amt}{bench_suffix}")
            } else {
                let strategy = action_to_strategy(&a);
                let f  = if a == "golden" { 50 } else { fast.get() };
                let sl = if a == "golden" { 200 } else { slow.get() };
                let rsi   = rsi_threshold.get();
                format!(
                    "/api/backtest?ticker={t}&strategy={strategy}&fast={f}&slow={sl}\
                     &years={years}&rsi_threshold={rsi}&initial_amount={amt}{bench_suffix}"
                )
            };
            busy.set(true); candidates.set(None);
            spawn_local(async move {
                single_result.set(Some(get_json::<BacktestResult>(&url).await));
                busy.set(false);
            });
        }
    };

    let run_selected_k = move |k: usize| {
        let tickers: Vec<String> = selected.get().into_iter().collect();
        let a = action.get();
        let use_pe   = pe_entry.get();
        let strategy = action_to_strategy(&a);
        let f  = if a == "golden" { 50 } else { fast.get() };
        let sl = if a == "golden" { 200 } else { slow.get() };
        let years = timeframe_years(&timeframe.get());
        let rsi   = rsi_threshold.get();
        let cached: HashSet<String> = pe_hist.get().keys().cloned().collect();
        pe_index.set(k); busy.set(true);
        spawn_local(async move {
            let mut out: Vec<(String, BacktestResult)> = Vec::new();
            let mut new_hist: Vec<(String, PeHistory)> = Vec::new();
            for t in tickers {
                let url = if use_pe {
                    format!("/api/backtest?ticker={t}&strategy=buy_and_hold&entry=pe_min&pe_index={k}")
                } else {
                    format!("/api/backtest?ticker={t}&strategy={strategy}&fast={f}&slow={sl}&years={years}&rsi_threshold={rsi}")
                };
                if let Ok(r) = get_json::<BacktestResult>(&url).await { out.push((t.clone(), r)); }
                if use_pe && !cached.contains(&t) {
                    if let Ok(h) = get_json::<PeHistory>(&format!("/api/pe_history?ticker={t}")).await {
                        new_hist.push((t, h));
                    }
                }
            }
            if !new_hist.is_empty() { pe_hist.update(|m| m.extend(new_hist)); }
            overlay.set(out); busy.set(false);
        });
    };

    view! {
        // ── Hero (full-bleed cover + teal scrim) ──────────────────────────────
        <section class="bd-grain" style="position:relative;overflow:hidden;min-height:100vh;\
                       background:var(--teal-700);display:flex;flex-direction:column;\
                       border-bottom:var(--border-bold) solid var(--ink-900);">
            // Full-bleed cover art
            <img src="/assets/hero-bg.png"
                 alt="An old bagholder leaning on a DeLorean in an empty teal landscape"
                 style="position:absolute;inset:0;width:100%;height:100%;object-fit:cover;\
                        object-position:right bottom;z-index:0;pointer-events:none;" />
            // Left→right teal gradient scrim keeps the copy legible over the art
            <div style="position:absolute;inset:0;z-index:1;pointer-events:none;\
                        background:linear-gradient(100deg,var(--teal-700) 4%,\
                        rgba(38,74,84,0.78) 30%,rgba(38,74,84,0.30) 52%,rgba(38,74,84,0) 70%);" />
            // Brand mark
            <header style="position:relative;z-index:3;display:flex;align-items:center;\
                           padding:24px 48px 4px;max-width:1320px;width:100%;margin:0 auto;box-sizing:border-box;">
                <img src="/assets/logo.png" alt="BagholderDeLorean"
                     style="height:72px;width:auto;display:block;" />
            </header>
            // Headline + copy over the art
            <div style="position:relative;z-index:2;flex:1;display:flex;align-items:center;\
                        max-width:1320px;width:100%;margin:0 auto;padding:12px 48px 48px;box-sizing:border-box;">
                <div style="max-width:560px;animation:bd-rise 0.55s var(--ease-out) both;">
                    <span style="display:inline-flex;align-items:center;gap:var(--space-2);\
                                 font-family:var(--font-mono);font-weight:700;font-size:var(--text-micro);\
                                 letter-spacing:0.16em;text-transform:uppercase;color:var(--ink-800);\
                                 background:var(--accent-soft);border:var(--border-line) solid var(--ink-900);\
                                 border-radius:var(--radius-full);padding:6px 13px;\
                                 box-shadow:var(--shadow-hard-sm);margin-bottom:24px;">
                        "Backtesting Time Machine"
                    </span>
                    <h1 style="font-family:var(--font-display);font-weight:800;\
                                font-size:clamp(42px,5vw,64px);line-height:0.98;\
                                letter-spacing:-0.03em;margin:0 0 18px;color:var(--paper-50);\
                                text-shadow:0 2px 18px rgba(20,38,44,0.45);">
                        "Backtest before" <br /> "you " <span style="color:var(--accent-soft);">"baghold."</span>
                    </h1>
                    <p style="font-size:18px;line-height:1.55;color:var(--text-on-ink-muted);\
                              max-width:440px;margin:0 0 4px;">
                        "Send a trading strategy back in time and find out whether you'd have \
                         gotten rich — or ended up holding the bag. Honest numbers, zero promises."
                    </p>
                    <p style="font-family:var(--font-mono);font-size:12px;\
                              color:var(--text-on-ink-muted);margin:24px 0 0;">
                        "Past performance is a vibe, not a promise."
                    </p>
                </div>
            </div>
            // Centred CTA with a bobbing chevron → Gallery section
            <button type="button" on:click=move |_| scroll_to("gallery")
                aria-label="Scroll to enter the gallery"
                style="position:relative;z-index:3;align-self:center;margin-bottom:26px;\
                       display:inline-flex;flex-direction:column;align-items:center;gap:6px;\
                       background:transparent;border:none;cursor:pointer;color:var(--paper-50);\
                       font-family:var(--font-mono);font-size:12px;font-weight:700;\
                       letter-spacing:0.14em;text-transform:uppercase;">
                "Scroll to enter the gallery"
                <span style="display:inline-flex;animation:bd-bob 1.6s var(--ease-out) infinite;">
                    <Icon name="chevron-down".to_string() size=24 />
                </span>
            </button>
        </section>

        // ── Gallery (placeholder — the wall of curated runs lands in #94) ─────
        <section id="gallery" style="min-height:100vh;display:flex;flex-direction:column;\
                       justify-content:center;padding:84px 56px;box-sizing:border-box;\
                       background:var(--surface-page);">
            <div style="max-width:1320px;margin:0 auto;width:100%;">
                <Overline>"Gallery of broken dreams"</Overline>
                <h2 style="font-family:var(--font-display);font-weight:800;font-size:36px;\
                           line-height:1.02;letter-spacing:-0.02em;color:var(--text-strong);margin:12px 0 8px;">
                    "A wall of broken dreams is coming"
                </h2>
                <p style="font-size:15px;color:var(--text-muted);margin:0;max-width:560px;">
                    "Curated backtests and your saved runs will live here. For now, scroll on \
                     to build one yourself."
                </p>
            </div>
        </section>

        // ── Configuration ────────────────────────────────────────────────────
        <section id="config" class="bd-grain" style="position:relative;overflow:hidden;\
                       min-height:100vh;display:flex;flex-direction:column;justify-content:center;\
                       padding:84px 56px;box-sizing:border-box;background:var(--teal-700);\
                       border-top:var(--border-bold) solid var(--ink-900);">
        <div style="position:relative;z-index:1;max-width:1320px;margin:0 auto;width:100%;\
                    display:flex;flex-direction:column;gap:var(--space-5);">

            // Section intro
            <header style="display:flex;flex-direction:column;">
                <Overline style="color:var(--accent-soft);margin-bottom:8px;">"Configure"</Overline>
                <h2 style="font-family:var(--font-display);font-weight:800;font-size:36px;\
                           line-height:1.02;letter-spacing:-0.02em;color:var(--paper-50);margin:0;">
                    "Build a backtest"
                </h2>
                <p style="font-size:15px;color:var(--text-on-ink-muted);margin:8px 0 0;">
                    "Choose what you trade and how you trade it, then send it back in time."
                </p>
            </header>

            // Datalist for ticker autocomplete — populated from /api/universe.
            {move || universe.get().map(|tickers| view! {
                <datalist id="tickers">
                    {tickers.into_iter().map(|t| view! { <option value=t /> }).collect_view()}
                </datalist>
            })}

            // Two-concern panel (left) + benchmark/tax add-ons (reserved right column)
            <div class="bd-config-grid" style="display:grid;gap:18px;align-items:start;">
            <BdCard padding="26px".to_string()>
            <section style="display:flex;flex-direction:column;gap:var(--space-4);">

                // ── Two concern panels ────────────────────────────────────────
                <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:14px;">
                    // 01 Stock selection
                    {move || {
                        let a    = action.get();
                        let prst = is_preset(&a);
                        let meth = sel_method.get();
                        view! {
                            <ConcernPanel step="01" title="Stock selection"
                                question="What am I trading?".to_string() disabled=prst>
                                {if prst {
                                    view! {
                                        <div style="font-size:13px;color:var(--text-muted);">
                                            {format!("🔒 Defined by the {} preset.", action_label(&a))}
                                        </div>
                                    }.into_view()
                                } else {
                                    view! {
                                        <div style="display:flex;flex-direction:column;gap:var(--space-3);">
                                            <BdTabs
                                                items=vec![
                                                    TabItem { value:"ticker".into(), label:"Single ticker".into() },
                                                    TabItem { value:"screen".into(), label:"Screen".into() },
                                                ]
                                                value=meth.clone()
                                                on_change=Box::new(move |v| {
                                                    sel_method.set(v);
                                                    candidates.set(None);
                                                    overlay.set(Vec::new());
                                                })
                                            />
                                            {if meth == "screen" {
                                                view! {
                                                    <BdSelect on_change=Box::new(move |v| screen_kind.set(v))>
                                                        <option value="lowpe">"Low P/E vs. industry"</option>
                                                        <option value="squeeze">"Short Squeeze (high short interest)"</option>
                                                        <option value="momentum_rank">"Momentum rank (6-month return)"</option>
                                                    </BdSelect>
                                                }.into_view()
                                            } else {
                                                view! {
                                                    <BdInput mono=true placeholder="AAPL".to_string()
                                                        value=ticker.get_untracked()
                                                        list="tickers".to_string()
                                                        on_input=Box::new(move |v| ticker.set(v.to_uppercase())) />
                                                }.into_view()
                                            }}
                                        </div>
                                    }.into_view()
                                }}
                            </ConcernPanel>
                        }
                    }}

                    // 02 Trade action
                    {move || {
                        let a    = action.get();
                        let prst = is_preset(&a);
                        let meme = is_meme(&a);
                        view! {
                            <ConcernPanel step="02" title="Trade action"
                                question="When do I get in & out?".to_string()>
                                <div style="display:flex;flex-direction:column;gap:var(--space-3);">
                                    <BdSelect on_change=Box::new(move |v| {
                                        action.set(v); single_result.set(None);
                                    })>
                                        <optgroup label="— ACTIONS (apply to your selection) —">
                                            <option value="buyhold">"Buy & Hold"</option>
                                            <option value="sma">"SMA Crossover"</option>
                                            <option value="golden">"Golden Cross / Death Cross"</option>
                                            <option value="btfd">"BTFD (Buy The Dip)"</option>
                                            <option value="meanrev">"Regime-Filtered Mean Reversion"</option>
                                        </optgroup>
                                        <optgroup label="— PRESETS (self-contained) —">
                                            <option value="pairs">"Pairs / Stat-Arb"</option>
                                            <option value="riskparity">"Risk Parity"</option>
                                            <option value="sectorrot">"Momentum Sector Rotation"</option>
                                            <option value="cycle">"Economic-Cycle Rotation"</option>
                                            <option value="cramer">"Inverse Cramer  ·  meme"</option>
                                            <option value="congress">"Congressional Copy-Trade  ·  meme"</option>
                                            <option value="short_squeeze">"Short Squeeze  ·  meme"</option>
                                        </optgroup>
                                    </BdSelect>
                                    <div style="display:flex;align-items:flex-start;gap:var(--space-2);flex-wrap:wrap;">
                                        {prst.then(|| view! { <BdBadge tone="accent".to_string()>"PRESET"</BdBadge> })}
                                        {meme.then(|| view! { <BdBadge tone="warn".to_string() soft=true>"MEME"</BdBadge> })}
                                        <span style="font-size:var(--text-xs);color:var(--text-muted);">
                                            {action_rationale(&a)}
                                        </span>
                                    </div>
                                </div>
                            </ConcernPanel>
                        }
                    }}
                </div>

                // ── 03 Parameters (conditional) ───────────────────────────────
                {move || {
                    let a = action.get();
                    let show = matches!(a.as_str(), "sma"|"golden"|"btfd"|"pairs"|"sectorrot"|"congress");
                    show.then(|| view! {
                        <div style="display:flex;flex-direction:column;gap:9px;">
                            <div style="display:flex;align-items:baseline;gap:7px;padding-left:2px;">
                                <span style="font-family:var(--font-mono);font-weight:700;font-size:var(--text-micro);color:var(--accent);">"03"</span>
                                <span style="font-weight:700;font-size:var(--text-micro);letter-spacing:0.1em;text-transform:uppercase;color:var(--text-strong);">"Parameters"</span>
                            </div>
                            <div style="padding:var(--space-4);background:var(--surface-sunken);\
                                        border:var(--border-line) solid var(--ink-800);border-radius:var(--radius-md);">
                                {match a.as_str() {
                                    "sma" | "golden" => view! {
                                        <div style="display:flex;gap:14px;flex-wrap:wrap;align-items:flex-end;">
                                            <div style="width:150px;">
                                                <BdInput label="Fast (days)".to_string() mono=true
                                                    value=fast.get().to_string()
                                                    on_input=Box::new(move |v| fast.set(v.parse().unwrap_or(20))) />
                                            </div>
                                            <div style="width:150px;">
                                                <BdInput label="Slow (days)".to_string() mono=true
                                                    value=slow.get().to_string()
                                                    on_input=Box::new(move |v| slow.set(v.parse().unwrap_or(50))) />
                                            </div>
                                            {(a == "golden").then(|| view! {
                                                <span style="font-size:var(--text-xs);color:var(--text-muted);align-self:center;">
                                                    "Preset of SMA Crossover (50/200). Inputs ignored."
                                                </span>
                                            })}
                                        </div>
                                    }.into_view(),
                                    "btfd" => view! {
                                        <div style="width:170px;">
                                            <BdInput label="RSI threshold".to_string() mono=true
                                                value=rsi_threshold.get().to_string()
                                                on_input=Box::new(move |v| rsi_threshold.set(v.parse().unwrap_or(20.0))) />
                                        </div>
                                    }.into_view(),
                                    "pairs" => view! {
                                        <div style="display:flex;gap:14px;flex-wrap:wrap;align-items:flex-end;">
                                            <div style="width:130px;">
                                                <BdInput label="Ticker A".to_string() mono=true
                                                    value=ticker_a.get()
                                                    on_input=Box::new(move |v| ticker_a.set(v.to_uppercase())) />
                                            </div>
                                            <div style="width:130px;">
                                                <BdInput label="Ticker B".to_string() mono=true
                                                    value=ticker_b.get()
                                                    on_input=Box::new(move |v| ticker_b.set(v.to_uppercase())) />
                                            </div>
                                            <div style="width:150px;">
                                                <BdInput label="Z-score entry".to_string() mono=true
                                                    value=entry_z.get().to_string()
                                                    on_input=Box::new(move |v| entry_z.set(v.parse().unwrap_or(2.0))) />
                                            </div>
                                        </div>
                                    }.into_view(),
                                    "sectorrot" => view! {
                                        <div style="width:190px;">
                                            <BdInput label="Sectors to hold (top N)".to_string() mono=true
                                                value=top_n.get().to_string()
                                                on_input=Box::new(move |v| top_n.set(v.parse().unwrap_or(3))) />
                                        </div>
                                    }.into_view(),
                                    _ => view! { // congress
                                        <div>
                                            <BdSwitch
                                                checked=realistic.get()
                                                label=if realistic.get() {
                                                    "Filing date (realistic)".to_string()
                                                } else {
                                                    "Transaction date (na\u{00ef}ve)".to_string()
                                                }
                                                on_change=Box::new(move |v| realistic.set(v))
                                            />
                                            <p style="margin:6px 0 0;font-size:var(--text-xs);color:var(--text-muted);">
                                                "Na\u{00ef}ve looks amazing. Realistic shows the edge already priced in."
                                            </p>
                                        </div>
                                    }.into_view(),
                                }}
                            </div>
                        </div>
                    })
                }}

                // ── Amount + Timeframe ────────────────────────────────────────
                {move || {
                    let a       = action.get();
                    let has_p03 = matches!(a.as_str(), "sma"|"golden"|"btfd"|"pairs"|"sectorrot"|"congress");
                    let step    = if has_p03 { "04" } else { "03" };
                    view! {
                        <div style="display:grid;\
                                    grid-template-columns:repeat(auto-fit,minmax(220px,1fr));\
                                    gap:14px;align-items:end;">
                            <div style="display:flex;flex-direction:column;gap:9px;max-width:220px;">
                                {field_heading(None, "Amount $", Some("How much do you put in?"))}
                                <BdInput mono=true placeholder="10000".to_string()
                                    value=format!("{:.0}", initial_amount.get_untracked())
                                    on_input=Box::new(move |v| {
                                        if let Ok(n) = v.parse::<f64>() {
                                            if n > 0.0 { initial_amount.set(n); }
                                        }
                                    }) />
                            </div>
                            <div style="display:flex;flex-direction:column;gap:9px;">
                                {field_heading(Some(step), "Timeframe", Some("How far back?"))}
                                <BdTabs full_width=true
                                    items=["1y","3y","5y","10y","Max"].iter().map(|t| TabItem {
                                        value: t.to_string(), label: t.to_string()
                                    }).collect()
                                    value=timeframe.get()
                                    on_change=Box::new(move |v| timeframe.set(v)) />
                                // Forward projection — a horizon input, companion to "how far back?".
                                {move || {
                                    let on = project_fwd.get();
                                    view! {
                                        <div style="display:flex;align-items:center;gap:10px;margin-top:2px;">
                                            <BdSwitch checked=on on_change=Box::new(move |v| project_fwd.set(v))
                                                label=(if on { "Project forward" } else { "Backtest only" }).to_string() />
                                            <span style="font-size:11.5px;color:var(--text-muted);font-style:italic;">"…and how far forward? (matches the backtest span)"</span>
                                        </div>
                                    }
                                }}
                            </div>
                        </div>
                    }
                }}

                // ── Run ───────────────────────────────────────────────────────
                {move || {
                    let a       = action.get();
                    let prst    = is_preset(&a);
                    let scr     = is_screen_run(&sel_method.get(), &a);
                    let is_busy = busy.get();
                    let lbl     = if is_busy { "Running\u{2026}" } else if prst { "Run preset" } else if scr { "Run screen" } else { "Run backtest" };
                    view! {
                        <div style="display:flex;justify-content:flex-end;">
                            <BdButton variant="primary".to_string() size="lg".to_string()
                                disabled=is_busy
                                on_click=Box::new(move || { run(); scroll_to("simulation"); })>
                                {lbl}
                            </BdButton>
                        </div>
                    }
                }}
            </section>
            </BdCard>

            // ── Add-ons (reserved right column): benchmark + tax ──────────────
            <div style="display:flex;flex-direction:column;gap:18px;">

                // Benchmark — OptionalPanel disclosure
                {move || {
                    if !show_benchmark.get() {
                        view! {
                            <button type="button" on:click=move |_| show_benchmark.set(true)
                                style="width:100%;display:flex;align-items:center;gap:14px;text-align:left;cursor:pointer;padding:14px 18px;background:var(--surface-card);border:2px dashed var(--ink-300);border-radius:var(--radius-md);color:var(--text-body);">
                                <span style="flex:none;width:38px;height:38px;border-radius:var(--radius-sm);background:var(--surface-sunken);border:2px solid var(--ink-800);display:flex;align-items:center;justify-content:center;color:var(--accent);">
                                    <Icon name="bar-chart-3".to_string() size=20 />
                                </span>
                                <span style="flex:1;">
                                    <span style="display:block;font-weight:700;font-size:14.5px;color:var(--text-strong);">"Add a benchmark"</span>
                                    <span style="display:block;font-size:12.5px;color:var(--text-muted);">"Overlay an index or asset to beat \u{2014} compare your strategy against buy-and-hold."</span>
                                </span>
                                <span style="flex:none;display:inline-flex;align-items:center;gap:6px;font-family:var(--font-mono);font-size:11px;letter-spacing:0.08em;text-transform:uppercase;color:var(--accent);">
                                    "Add" <Icon name="plus".to_string() size=16 />
                                </span>
                            </button>
                        }.into_view()
                    } else {
                        view! {
                            <div style="border:2px solid var(--ink-800);border-radius:var(--radius-md);background:var(--surface-card);box-shadow:var(--shadow-hard-sm);overflow:hidden;">
                                <div style="display:flex;align-items:center;gap:10px;padding:12px 16px;border-bottom:2px solid var(--ink-800);background:var(--surface-sunken);">
                                    <div style="flex:1;min-width:0;">
                                        <div style="display:flex;align-items:center;gap:7px;min-height:24px;">
                                            <span style="font-family:var(--font-mono);font-weight:700;font-size:11px;color:var(--accent);">"05"</span>
                                            <span style="font-weight:700;font-size:11px;letter-spacing:0.1em;text-transform:uppercase;color:var(--text-strong);white-space:nowrap;">"Benchmark"</span>
                                        </div>
                                        <span style="font-size:11.5px;color:var(--text-muted);font-style:italic;">"Compare against what?"</span>
                                    </div>
                                    <button type="button" aria-label="Remove benchmark"
                                        on:click=move |_| show_benchmark.set(false)
                                        style="flex:none;display:inline-flex;align-items:center;gap:5px;padding:6px 10px;background:transparent;border:2px solid var(--paper-300);border-radius:var(--radius-sm);cursor:pointer;font-size:12px;font-weight:600;color:var(--text-muted);">
                                        <Icon name="x".to_string() size=14 /> "Remove"
                                    </button>
                                </div>
                                <div style="padding:16px;display:flex;flex-direction:column;gap:12px;">
                                    <BdInput mono=true label="vs. ticker".to_string()
                                        placeholder="SPY".to_string()
                                        value=benchmark_ticker.get()
                                        on_input=Box::new(move |v| benchmark_ticker.set(v.trim().to_uppercase())) />
                                    <BdSelect label="vs. strategy".to_string()
                                        on_change=Box::new(move |v| benchmark_strategy.set(v))>
                                        <option value="buy_and_hold">"Buy and hold"</option>
                                        <option value="sma_crossover">"SMA crossover (20/50)"</option>
                                    </BdSelect>
                                </div>
                            </div>
                        }.into_view()
                    }
                }}

                // Tax simulation — collapsed CTA → expandable "06" card (TaxSim.jsx)
                {move || {
                            let tax_sys = tax_system.get();
                            if !tax_enabled.get() {
                                view! {
                                    <button type="button" on:click=move |_| tax_enabled.set(true)
                                        style="width:100%;display:flex;align-items:center;gap:14px;text-align:left;cursor:pointer;padding:14px 18px;background:var(--surface-card);border:2px dashed var(--ink-300);border-radius:var(--radius-md);color:var(--text-body);">
                                        <span style="flex:none;width:38px;height:38px;border-radius:var(--radius-sm);background:var(--surface-sunken);border:2px solid var(--ink-800);display:flex;align-items:center;justify-content:center;color:var(--accent);">
                                            <Icon name="receipt".to_string() size=20 />
                                        </span>
                                        <span style="flex:1;">
                                            <span style="display:block;font-weight:700;font-size:14.5px;color:var(--text-strong);">"Add tax simulation"</span>
                                            <span style="display:block;font-size:12.5px;color:var(--text-muted);">"See what you actually keep after the taxman \u{2014} U.S. or German capital-gains rules."</span>
                                        </span>
                                        <span style="flex:none;display:inline-flex;align-items:center;gap:6px;font-family:var(--font-mono);font-size:11px;letter-spacing:0.08em;text-transform:uppercase;color:var(--accent);">
                                            "Set up" <Icon name="plus".to_string() size=16 />
                                        </span>
                                    </button>
                                }.into_view()
                            } else {
                                let collapsed = tax_collapsed.get();
                                let header_border = if collapsed { "none" } else { "2px solid var(--ink-800)" };
                                let sellall_active = tax_sys != "none";
                                let sellall_de = tax_sys == "de";
                                let badge = (tax_sys != "none").then(|| {
                                    let lbl = if tax_sys == "us" { "United States" } else { "Germany" };
                                    view! { <BdBadge tone="accent".to_string() soft=true>{lbl}</BdBadge> }
                                });
                                view! {
                                    <div style="border:2px solid var(--ink-800);border-radius:var(--radius-md);background:var(--surface-card);box-shadow:var(--shadow-hard-sm);overflow:hidden;">
                                        <div style=format!("display:flex;align-items:center;gap:10px;padding:12px 16px;border-bottom:{header_border};background:var(--surface-sunken);")>
                                            <div style="flex:1;">
                                                <div style="display:flex;align-items:center;gap:7px;min-height:24px;">
                                                    <span style="font-family:var(--font-mono);font-weight:700;font-size:11px;color:var(--accent);">"06"</span>
                                                    <span style="font-weight:700;font-size:11px;letter-spacing:0.1em;text-transform:uppercase;color:var(--text-strong);white-space:nowrap;">"Tax simulation"</span>
                                                    {badge}
                                                </div>
                                                <span style="font-size:11.5px;color:var(--text-muted);font-style:italic;">"What does the taxman leave you?"</span>
                                            </div>
                                            <button type="button" aria-label="Remove tax simulation"
                                                on:click=move |_| { tax_enabled.set(false); tax_collapsed.set(false); tax_system.set("none".to_string()); }
                                                style="flex:none;display:inline-flex;align-items:center;gap:5px;padding:6px 10px;background:transparent;border:2px solid var(--paper-300);border-radius:var(--radius-sm);cursor:pointer;font-size:12px;font-weight:600;color:var(--text-muted);">
                                                <Icon name="x".to_string() size=14 /> "Remove"
                                            </button>
                                            <button type="button"
                                                aria-expanded=(!collapsed).to_string()
                                                aria-label=if collapsed { "Expand tax simulation" } else { "Collapse tax simulation" }
                                                on:click=move |_| tax_collapsed.update(|c| *c = !*c)
                                                style="flex:none;display:inline-flex;align-items:center;justify-content:center;width:30px;height:30px;background:transparent;border:2px solid var(--paper-300);border-radius:var(--radius-sm);cursor:pointer;color:var(--text-muted);">
                                                <Icon name=if collapsed { "chevron-down".to_string() } else { "chevron-up".to_string() } size=16 />
                                            </button>
                                        </div>
                                        {(!collapsed).then(|| view! {
                                            <div style="padding:16px;display:flex;flex-direction:column;gap:16px;">
                                                <div style="display:flex;flex-direction:column;gap:9px;">
                                                    {field_heading(None, "Tax system", Some("Pick the regime your gains are taxed under."))}
                                                    <BdTabs full_width=true
                                                        items=vec![
                                                            TabItem { value: "none".to_string(), label: "None".to_string() },
                                                            TabItem { value: "us".to_string(), label: "United States".to_string() },
                                                            TabItem { value: "de".to_string(), label: "Germany".to_string() },
                                                        ]
                                                        value=tax_sys.clone()
                                                        on_change=Box::new(move |v| tax_system.set(v)) />
                                                </div>
                                                {sellall_active.then(move || view! {
                                                    <div style="display:flex;align-items:center;gap:14px;padding:12px 16px;background:var(--surface-sunken);border:2px solid var(--ink-800);border-radius:var(--radius-md);">
                                                        <div style="flex:1;">
                                                            <span style="display:block;font-weight:700;font-size:12.5px;color:var(--text-strong);">"Sell everything at the end"</span>
                                                            {move || {
                                                                let on = tax_sellall.get();
                                                                let copy = if on {
                                                                    "Liquidate the whole position in the final year \u{2014} all remaining gains get realized and taxed.".to_string()
                                                                } else if sellall_de {
                                                                    "Keep holding the bags \u{2014} unrealized gains are never taxed (Vorabpauschale advances still accrue).".to_string()
                                                                } else {
                                                                    "Keep holding the bags \u{2014} unrealized gains are never taxed.".to_string()
                                                                };
                                                                view! { <span style="display:block;margin-top:2px;font-size:11.5px;color:var(--text-muted);">{copy}</span> }
                                                            }}
                                                        </div>
                                                        {move || { let on = tax_sellall.get(); view! { <BdSwitch checked=on on_change=Box::new(move |v| tax_sellall.set(v)) label=(if on { "On" } else { "Off" }).to_string() /> } }}
                                                    </div>
                                                })}
                                                {(tax_sys == "none").then(|| view! {
                                                    <div style="display:flex;align-items:center;gap:10px;padding:14px 16px;background:var(--surface-sunken);border:2px solid var(--ink-800);border-radius:var(--radius-md);font-size:13px;color:var(--text-muted);">
                                                        <Icon name="minus-circle".to_string() size=16 />
                                                        "No tax applied \u{2014} results stay pre-tax, exactly as before."
                                                    </div>
                                                })}
                                                {(tax_sys == "us").then(move || us_knobs(tax_income))}
                                                {(tax_sys == "de").then(move || de_knobs(tax_allowance, tax_church, tax_vorab, tax_estimate, tax_teilfrei))}
                                            </div>
                                        })}
                                    </div>
                                }.into_view()
                            }
                        }}
            </div>
            </div>
        </div>
        </section>

        // ── Simulation ────────────────────────────────────────────────────────
        <section id="simulation" style="min-height:100vh;display:flex;flex-direction:column;\
                       padding:84px 56px 56px;box-sizing:border-box;background:var(--surface-sunken);\
                       border-top:var(--border-bold) solid var(--ink-900);">
            <header style="display:flex;align-items:flex-end;justify-content:space-between;gap:20px;\
                           margin:0 auto 22px;max-width:1320px;width:100%;flex-wrap:wrap;">
                <div>
                    <Overline>"Simulation"</Overline>
                    <h2 style="font-family:var(--font-display);font-weight:800;font-size:36px;\
                               line-height:1.02;letter-spacing:-0.02em;color:var(--text-strong);margin:0;">
                        "The verdict"
                    </h2>
                </div>
                {move || {
                    let ran = single_result.get().is_some() || candidates.get().is_some();
                    (ran && !busy.get()).then(|| view! {
                        <BdButton variant="secondary".to_string() size="md".to_string()
                            on_click=Box::new(|| scroll_to("config"))>
                            <Icon name="sliders-horizontal".to_string() size=16 /> "Back to configure"
                        </BdButton>
                    })
                }}
            </header>
            <div style=move || {
                let ran = single_result.get().is_some() || candidates.get().is_some();
                let justify = if ran && !busy.get() { "flex-start" } else { "center" };
                format!("flex:1;min-height:0;max-width:1320px;width:100%;margin:0 auto;\
                         display:flex;flex-direction:column;justify-content:{justify};")
            }>

            // ── Results ───────────────────────────────────────────────────────
            {move || {
                let is_busy = busy.get();
                match (single_result.get(), candidates.get()) {
                    (None, None) if is_busy => {
                        // Screen runs warm ~20 names from the net; ticker runs replay one tape.
                        let screen = is_screen_run(&sel_method.get(), &action.get());
                        let (title, sub) = if screen {
                            ("Warming up \u{2014} first run takes a moment", "Fetching \u{223c}20 stocks from across the timeline.")
                        } else {
                            ("Spinning up the flux capacitor\u{2026}", "Replaying the tape tick by tick.")
                        };
                        view! {
                            <BdCard><div style="text-align:center;padding:var(--space-6) var(--space-4);">
                                <span style="display:inline-block;width:34px;height:34px;\
                                             border-radius:var(--radius-full);\
                                             border:4px solid var(--paper-300);border-top-color:var(--accent);\
                                             animation:bd-spin 0.8s linear infinite;margin-bottom:var(--space-4);" />
                                <p style="font-family:var(--font-display);font-weight:var(--weight-bold);\
                                          font-size:var(--text-lg);color:var(--text-strong);margin:0 0 var(--space-1);">
                                    {title}
                                </p>
                                <p style="font-size:var(--text-sm);color:var(--text-muted);margin:0;">
                                    {sub}
                                </p>
                            </div></BdCard>
                        }.into_view()
                    },

                    (Some(Err(e)), _) => view! {
                        <BdCallout tone="loss".to_string() icon="alert-triangle".to_string()
                            title="That didn\u{2019}t work".to_string()>{e}</BdCallout>
                    }.into_view(),

                    (Some(Ok(r)), _) => view! {
                        <div class="bd-fade" style="width:100%;">
                            {equity_single(&r, action_label(&action.get()))}
                        </div>
                    }.into_view(),

                    (None, Some(Err(e))) => view! {
                        <BdCallout tone="loss".to_string() icon="alert-triangle".to_string()
                            title="The screen choked".to_string()>{e}</BdCallout>
                    }.into_view(),

                    (None, Some(Ok(cands))) => {
                        let a           = action.get();
                        let show_pe_tog = screen_kind.get() == "lowpe";
                        let bt_dis      = is_busy || selected.with(|s| s.is_empty());

                        let rows = cands.iter().map(|c| {
                            // Pre-clone all data from c so view! closures are 'static
                            let t_box = c.ticker.clone();   // checkbox checked read
                            let t_row = c.ticker.clone();   // row highlight read
                            let tdis = c.ticker.clone();
                            let ind  = c.industry.clone();
                            let pe_s = format!("{:.1}", c.pe);
                            let ipe  = format!("{:.1}", c.industry_median_pe);
                            let rpe  = format!("{:.2}", c.relative_pe);
                            view! {
                                <tr style=move || format!(
                                    "background:{};border-bottom:var(--border-hair) solid var(--border-soft);",
                                    if selected.with(|s| s.contains(&t_row)) { "var(--surface-sunken)" }
                                    else { "transparent" }
                                )>
                                    <td style="padding:11px 6px;width:34px;">
                                        {move || {
                                            let on = selected.with(|s| s.contains(&t_box));
                                            let t  = t_box.clone();
                                            view! {
                                                <BdCheckbox checked=on
                                                    on_change=Box::new(move |_| selected.update(|s| {
                                                        if !s.remove(&t) { s.insert(t.clone()); }
                                                    })) />
                                            }
                                        }}
                                    </td>
                                    <td style="padding:11px 6px;font-family:var(--font-mono);font-weight:700;\
                                               font-size:13.5px;color:var(--text-strong);">{tdis}</td>
                                    <td style="padding:11px 6px;font-size:13px;color:var(--text-muted);">{ind}</td>
                                    <td style="padding:11px 6px;text-align:right;font-family:var(--font-mono);font-size:13px;">{pe_s}</td>
                                    <td style="padding:11px 6px;text-align:right;font-family:var(--font-mono);font-size:13px;color:var(--text-muted);">{ipe}</td>
                                    <td style="padding:11px 6px;text-align:right;">
                                        <BdBadge tone="gain".to_string() soft=true>{rpe}</BdBadge>
                                    </td>
                                </tr>
                            }
                        }).collect_view();

                        view! {
                            <div class="bd-fade" style="display:flex;flex-direction:column;gap:18px;">
                                {is_busy.then(|| view! {
                                    <BdCallout tone="neutral".to_string() title="Warming up\u{2026}".to_string()>
                                        "First run fetches \u{223c}23 names from the internet \u{2014} about 2 minutes. Cached after that."
                                    </BdCallout>
                                })}

                                <BdCard overline="Low P/E vs. industry".to_string()
                                        title="Screen results".to_string()>
                                    <div style="overflow-x:auto;">
                                        <table style="border-collapse:collapse;width:100%;min-width:500px;">
                                            <thead>
                                                <tr style="border-bottom:var(--border-line) solid var(--border-soft);">
                                                    <th style="padding:0 6px 10px;width:34px;" />
                                                    <th class="bd-overline" style="padding:0 6px 10px;text-align:left;">"Ticker"</th>
                                                    <th class="bd-overline" style="padding:0 6px 10px;text-align:left;">"Industry"</th>
                                                    <th class="bd-overline" style="padding:0 6px 10px;text-align:right;">"P/E"</th>
                                                    <th class="bd-overline" style="padding:0 6px 10px;text-align:right;">"Ind. med."</th>
                                                    <th class="bd-overline" style="padding:0 6px 10px;text-align:right;">"Rel. P/E"</th>
                                                </tr>
                                            </thead>
                                            <tbody>{rows}</tbody>
                                        </table>
                                    </div>
                                    <div style="display:flex;align-items:center;justify-content:space-between;\
                                                gap:var(--space-4);margin-top:18px;flex-wrap:wrap;">
                                        {if show_pe_tog {
                                            view! {
                                                <BdSwitch checked=pe_entry.get()
                                                    label="Enter at P/E trough".to_string()
                                                    on_change=Box::new(move |v| {
                                                        pe_entry.set(v);
                                                        overlay.set(Vec::new());
                                                        pe_hist.update(|m| m.clear());
                                                    }) />
                                            }.into_view()
                                        } else {
                                            view! {
                                                <span style="font-size:12.5px;color:var(--text-muted);">
                                                    "Applying " <strong>{action_label(&a)}</strong> " to each selected name."
                                                </span>
                                            }.into_view()
                                        }}
                                        <BdButton variant="primary".to_string() disabled=bt_dis
                                            on_click=Box::new(move || run_selected_k(0))>
                                            "Backtest selected"
                                        </BdButton>
                                    </div>
                                </BdCard>

                                // Trough stepper (pe_entry only, when results exist)
                                {move || {
                                    let maxn = overlay.get().iter().filter_map(|(_, r)| r.entry_count).max();
                                    if let (true, Some(n)) = (pe_entry.get(), maxn) {
                                        if n > 1 {
                                            let k     = pe_index.get();
                                            let n_dis = busy.get() || k == 0;
                                            let o_dis = busy.get() || k + 1 >= n;
                                            let ctr   = format!("Trough {} of {} (0 = most recent)", k + 1, n);
                                            return view! {
                                                <div style="display:flex;align-items:center;\
                                                            justify-content:center;gap:var(--space-3);">
                                                    <BdButton variant="dark".to_string() size="sm".to_string()
                                                        disabled=n_dis
                                                        on_click=Box::new(move || run_selected_k(pe_index.get().saturating_sub(1)))>
                                                        "\u{25c4} Newer"
                                                    </BdButton>
                                                    <span style="font-family:var(--font-mono);font-weight:700;\
                                                                 font-size:var(--text-sm);color:var(--text-strong);\
                                                                 min-width:170px;text-align:center;">{ctr}</span>
                                                    <BdButton variant="dark".to_string() size="sm".to_string()
                                                        disabled=o_dis
                                                        on_click=Box::new(move || run_selected_k(pe_index.get() + 1))>
                                                        "Older \u{25ba}"
                                                    </BdButton>
                                                </div>
                                            }.into_view();
                                        }
                                    }
                                    view! { <></> }.into_view()
                                }}

                                // Overlay chart
                                {move || {
                                    let o = overlay.get();
                                    if o.is_empty() { return view! { <></> }.into_view(); }
                                    let ol = format!("{} · {} · $10,000 each", action_label(&action.get()),
                                        if pe_entry.get() { "P/E trough entry".to_string() } else { timeframe.get() });
                                    view! {
                                        <BdCard tone="dark".to_string() overline=ol
                                                title="Overlaid backtest".to_string()>
                                            <div style="margin-top:6px;">{equity_overlay(&o)}</div>
                                        </BdCard>
                                    }.into_view()
                                }}

                                // P/E mini-charts
                                {move || {
                                    let results = overlay.get();
                                    let hist    = pe_hist.get();
                                    if !pe_entry.get() || results.is_empty() { return view! { <></> }.into_view(); }
                                    let charts = results.iter()
                                        .filter_map(|(t, r)| hist.get(t).map(|h| pe_chart(t, h, r.entry_date)))
                                        .collect_view();
                                    view! {
                                        <div>
                                            <p class="bd-overline" style="margin:0 0 var(--space-3);letter-spacing:var(--tracking-overline);">
                                                "P/E over time \u{2014} dots are troughs, red is your entry"
                                            </p>
                                            <div style="display:grid;\
                                                        grid-template-columns:repeat(auto-fill,minmax(200px,1fr));\
                                                        gap:var(--space-3);">{charts}</div>
                                            <p style="font-size:var(--text-xs);color:var(--text-muted);margin:10px 0 0;text-align:center;">
                                                "Step back to ask: what if I\u{2019}d bought the " <em>"previous"</em>
                                                " time it looked this cheap?"
                                            </p>
                                        </div>
                                    }.into_view()
                                }}
                            </div>
                        }.into_view()
                    }

                    _ => view! {
                        <BdCard><div style="text-align:center;padding:var(--space-6) var(--space-4);color:var(--text-muted);">
                            <span style="display:inline-flex;width:56px;height:56px;\
                                         border-radius:var(--radius-full);background:var(--surface-sunken);\
                                         border:var(--border-line) solid var(--ink-800);color:var(--ink-500);\
                                         align-items:center;justify-content:center;\
                                         margin-bottom:14px;">
                                <Icon name="rewind".to_string() size=26 />
                            </span>
                            <p style="font-family:var(--font-display);font-weight:var(--weight-bold);\
                                      font-size:20px;color:var(--text-strong);margin:0 0 var(--space-1);">
                                "Nothing run yet."
                            </p>
                            <p style="font-size:13.5px;color:var(--text-muted);margin:0 0 18px;">
                                "Open a backtest from the gallery or build one, then send it back in time."
                            </p>
                            <BdButton variant="secondary".to_string() size="md".to_string()
                                on_click=Box::new(|| scroll_to("config"))>
                                <Icon name="sliders-horizontal".to_string() size=16 /> "Go to the configurator"
                            </BdButton>
                        </div></BdCard>
                    }.into_view(),
                }
            }}
            </div>
        </section>

        <BdSiteFooter
            image="/assets/footer.png".to_string()
            tagline="Where we\u{2019}re going, we don\u{2019}t need returns".to_string()
            links=vec![
                FooterLink { label: "About".to_string(),        href: "#about".to_string() },
                FooterLink { label: "Imprint".to_string(),      href: "#imprint".to_string() },
                FooterLink { label: "Legal Notice".to_string(), href: "#legal".to_string() },
            ]
        />
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount_to_body(App);
}
