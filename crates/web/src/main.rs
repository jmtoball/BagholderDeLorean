//! Bagholder DeLorean — two-concern backtesting UI.
//! Stock selection (what to trade) × Trade action (when to get in/out).
//! Presets bypass the two-panel structure when selection and action are inseparable.

pub mod components;

use std::collections::{HashMap, HashSet};

use bagholder_core::{BacktestResult, Candidate, PeHistory};
use chrono::{Datelike, NaiveDate};
use leptos::*;
use serde::de::DeserializeOwned;

use components::{BdBadge, BdButton, BdCallout, BdCard, BdInput, BdSelect, BdStat, BdSwitch, BdTabs, TabItem};

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
        "cramer"     => "Inverse Cramer",
        "congress"   => "Congressional Copy-Trade",
        _            => "",
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
        "cramer"     => "Selection is Cramer's picks; the action is to fade them. Inseparable, by design.",
        "congress"   => "Mirror disclosed politician trades. Naively spectacular — until you wait for the filing date.",
        _            => "",
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
fn is_meme(id: &str) -> bool { matches!(id, "cramer" | "congress") }
fn timeframe_years(tf: &str) -> u32 {
    match tf { "1y" => 1, "3y" => 3, "5y" => 5, "10y" => 10, _ => 0 }
}

// ─── Formatting ───────────────────────────────────────────────────────────────

fn fmt_pct(x: f64) -> String {
    let v = x * 100.0;
    format!("{}{:.1}%", if v >= 0.0 { "+" } else { "\u{2212}" }, v.abs())
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
fn equity_single(r: &BacktestResult, label: &str) -> View {
    let Some(pts) = to_pts(r) else {
        return view! { <p style="color:var(--text-on-ink-muted);">"Not enough data."</p> }.into_view();
    };
    let line = svg_path(&pts);
    let (fx, _) = pts[0];
    let (lx, _) = *pts.last().unwrap();
    let h_bot   = format!("{:.1}", H - PAD);
    let area    = format!("{} L{:.1},{h_bot} L{:.1},{h_bot} Z", line, lx, fx);

    let win          = r.metrics.total_return >= 0.0;
    let color        = if win { "var(--gain-200)" } else { "var(--loss-200)" };
    let total_ret    = fmt_pct(r.metrics.total_return);
    let total_ret_s  = total_ret.clone(); // for BdStat (badge below owns original)
    let badge_tone   = if win { "gain" } else { "loss" };
    let card_title   = if win { "You'd have made money" } else { "You'd have lost money" }.to_string();
    let card_ol      = format!("{label} · starts at $10,000");
    let cagr_str     = format!("{} /yr", fmt_pct(r.metrics.cagr));
    let mdd_str      = fmt_pct(r.metrics.max_drawdown);
    let sharpe_str   = format!("{:.2}", r.metrics.sharpe);
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

    view! {
        <div style="display:flex;flex-direction:column;gap:16px;">
            <div style="display:grid;grid-template-columns:repeat(4,1fr);gap:12px;">
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
            </div>

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
                              stroke="rgba(246,241,228,0.10)" stroke-width="1" stroke-dasharray="3 5" />
                        <line x1=x1s.clone() x2=x2s.clone() y1=gy2.clone() y2=gy2
                              stroke="rgba(246,241,228,0.10)" stroke-width="1" stroke-dasharray="3 5" />
                        <line x1=x1s x2=x2s y1=gy3.clone() y2=gy3
                              stroke="rgba(246,241,228,0.10)" stroke-width="1" stroke-dasharray="3 5" />
                        <path d=area fill="url(#bd_eq_grad)" />
                        <path d=line fill="none" stroke=color stroke-width="2.5"
                              stroke-linejoin="round" stroke-linecap="round" />
                    </svg>
                    <div style="display:flex;gap:18px;margin-top:12px;font-size:12px;\
                                color:var(--text-on-ink-muted);font-family:var(--font-mono);">
                        <span style="display:inline-flex;align-items:center;gap:7px;">
                            <span style=sw />"Strategy"
                        </span>
                    </div>
                </div>
            </BdCard>

            {bag.then(|| view! {
                <div style="display:flex;gap:14px;align-items:flex-start;padding:18px 20px;\
                            background:var(--loss-200);border:3px solid var(--ink-900);\
                            border-radius:var(--radius-lg);box-shadow:var(--shadow-hard);">
                    <span style="flex:none;width:44px;height:44px;border-radius:50%;\
                                 background:var(--loss);border:2px solid var(--ink-900);\
                                 display:flex;align-items:center;justify-content:center;\
                                 font-size:22px;line-height:1;">"🛍"</span>
                    <div>
                        <div style="font-family:var(--font-display);font-weight:800;font-size:19px;\
                                    letter-spacing:-0.01em;color:var(--loss-600);margin-bottom:3px;">
                            "Congratulations, you're a bagholder."
                        </div>
                        <p style="margin:0;font-size:14px;line-height:1.5;color:var(--text-body);">
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
                "Excludes taxes, slippage, and survivorship bias. \
                 Past performance is a vibe, not a promise."
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
        return view! { <p style="color:var(--text-on-ink-muted);">"Not enough data."</p> }.into_view();
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
            <span style="display:inline-flex;align-items:center;gap:8px;\
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
                <line x1=x1s.clone() x2=x2s.clone() y1=gy1.clone() y2=gy1 stroke="rgba(246,241,228,0.10)" stroke-width="1" stroke-dasharray="3 5" />
                <line x1=x1s.clone() x2=x2s.clone() y1=gy2.clone() y2=gy2 stroke="rgba(246,241,228,0.10)" stroke-width="1" stroke-dasharray="3 5" />
                <line x1=x1s x2=x2s y1=gy3.clone() y2=gy3 stroke="rgba(246,241,228,0.10)" stroke-width="1" stroke-dasharray="3 5" />
                {lines}
            </svg>
            <div style="display:flex;flex-wrap:wrap;gap:16px;margin-top:14px;">{legend}</div>
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
        <div style="background:var(--teal-600);border:2px solid var(--ink-900);\
                    border-radius:var(--radius-md);padding:12px 14px;">
            <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:6px;">
                <span style="font-family:var(--font-mono);font-weight:700;font-size:13px;color:var(--paper-50);">
                    {ticker.to_string()}
                </span>
                <span style="font-size:11px;color:var(--text-on-ink-muted);">{lg}</span>
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
        "display:flex;flex-direction:column;padding:16px;\
         background:var(--surface-sunken);border:2px solid var(--ink-800);\
         border-radius:var(--radius-md);min-height:104px;justify-content:center;{}",
        if disabled { "opacity:0.4;pointer-events:none;filter:saturate(0.5);" } else { "" }
    );
    view! {
        <div style="display:flex;flex-direction:column;gap:9px;">
            <div style="display:flex;flex-direction:column;gap:2px;padding-left:2px;">
                <div style="display:flex;align-items:baseline;gap:7px;">
                    <span style="font-family:var(--font-mono);font-weight:700;font-size:11px;\
                                 color:var(--accent);">{step}</span>
                    <span style="font-weight:700;font-size:11px;letter-spacing:0.1em;\
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
        let use_screen = sel_method.get() == "screen" && !prst;
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
            let url = if a == "congress" {
                format!(
                    "/api/backtest?ticker={t}&strategy=congress_copy_trade&year=2023\
                     &use_filing_date={}&years={years}",
                    realistic.get()
                )
            } else if a == "cramer" {
                format!("/api/backtest?ticker={t}&strategy=cramer_inverse&years={years}")
            } else {
                let strategy = action_to_strategy(&a);
                let f  = if a == "golden" { 50 } else { fast.get() };
                let sl = if a == "golden" { 200 } else { slow.get() };
                let rsi   = rsi_threshold.get();
                format!(
                    "/api/backtest?ticker={t}&strategy={strategy}&fast={f}&slow={sl}\
                     &years={years}&rsi_threshold={rsi}"
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
        <main style="max-width:820px;margin:2rem auto;padding:0 var(--gutter);\
                     display:flex;flex-direction:column;gap:28px;">

            <header>
                <h1 style="font-size:var(--text-display-sm);letter-spacing:var(--tracking-tight);\
                            margin-bottom:4px;">"Bagholder DeLorean"</h1>
                <p style="font-size:var(--text-sm);color:var(--text-muted);margin:0;">
                    "What if you'd invested differently? Let's find out — and rub it in."
                </p>
            </header>

            <section style="display:flex;flex-direction:column;gap:16px;">

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
                                        <div style="display:flex;flex-direction:column;gap:12px;">
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
                                <div style="display:flex;flex-direction:column;gap:12px;">
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
                                        </optgroup>
                                    </BdSelect>
                                    <div style="display:flex;align-items:flex-start;gap:8px;flex-wrap:wrap;">
                                        {prst.then(|| view! { <BdBadge tone="accent".to_string()>"PRESET"</BdBadge> })}
                                        {meme.then(|| view! { <BdBadge tone="warn".to_string() soft=true>"MEME"</BdBadge> })}
                                        <span style="font-size:12px;color:var(--text-muted);">
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
                                <span style="font-family:var(--font-mono);font-weight:700;font-size:11px;color:var(--accent);">"03"</span>
                                <span style="font-weight:700;font-size:11px;letter-spacing:0.1em;text-transform:uppercase;color:var(--text-strong);">"Parameters"</span>
                            </div>
                            <div style="padding:16px;background:var(--surface-sunken);\
                                        border:2px solid var(--ink-800);border-radius:var(--radius-md);">
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
                                                <span style="font-size:12px;color:var(--text-muted);align-self:center;">
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
                                            <p style="margin:6px 0 0;font-size:12px;color:var(--text-muted);">
                                                "Na\u{00ef}ve looks amazing. Realistic shows the edge already priced in."
                                            </p>
                                        </div>
                                    }.into_view(),
                                }}
                            </div>
                        </div>
                    })
                }}

                // ── Timeframe + Run ───────────────────────────────────────────
                {move || {
                    let a       = action.get();
                    let has_p03 = matches!(a.as_str(), "sma"|"golden"|"btfd"|"pairs"|"sectorrot"|"congress");
                    let step    = if has_p03 { "04" } else { "03" };
                    let prst    = is_preset(&a);
                    let scr     = sel_method.get() == "screen" && !prst;
                    let is_busy = busy.get();
                    let lbl     = if is_busy { "Running\u{2026}" } else if prst { "Run preset" } else if scr { "Run screen" } else { "Run backtest" };
                    view! {
                        <div style="display:flex;gap:16px;flex-wrap:wrap;align-items:flex-end;">
                            <div style="flex:1 1 280px;min-width:200px;display:flex;flex-direction:column;gap:9px;">
                                <div style="display:flex;align-items:baseline;gap:7px;padding-left:2px;">
                                    <span style="font-family:var(--font-mono);font-weight:700;font-size:11px;color:var(--accent);">{step}</span>
                                    <span style="font-weight:700;font-size:11px;letter-spacing:0.1em;text-transform:uppercase;color:var(--text-strong);">"Timeframe"</span>
                                </div>
                                <BdTabs full_width=true
                                    items=["1y","3y","5y","10y","Max"].iter().map(|t| TabItem {
                                        value: t.to_string(), label: t.to_string()
                                    }).collect()
                                    value=timeframe.get()
                                    on_change=Box::new(move |v| timeframe.set(v)) />
                            </div>
                            <BdButton variant="primary".to_string() size="lg".to_string()
                                disabled=is_busy on_click=Box::new(run)>
                                {lbl}
                            </BdButton>
                        </div>
                    }
                }}
            </section>

            // ── Results ───────────────────────────────────────────────────────
            {move || {
                let is_busy = busy.get();
                match (single_result.get(), candidates.get()) {
                    (None, None) if is_busy => view! {
                        <div style="text-align:center;padding:var(--space-7) 0;color:var(--text-muted);">
                            "\u{231b} Computing your wealth destruction\u{2026}"
                        </div>
                    }.into_view(),

                    (Some(Err(e)), _) => view! {
                        <BdCallout tone="loss".to_string() title="Error".to_string()>{e}</BdCallout>
                    }.into_view(),

                    (Some(Ok(r)), _) => equity_single(&r, action_label(&action.get())),

                    (None, Some(Err(e))) => view! {
                        <BdCallout tone="loss".to_string() title="Screen error".to_string()>{e}</BdCallout>
                    }.into_view(),

                    (None, Some(Ok(cands))) => {
                        let a           = action.get();
                        let show_pe_tog = screen_kind.get() == "lowpe";
                        let bt_dis      = is_busy || selected.with(|s| s.is_empty());

                        let rows = cands.iter().map(|c| {
                            // Pre-clone all data from c so view! closures are 'static
                            let t1   = c.ticker.clone();
                            let t2   = c.ticker.clone();
                            let tdis = c.ticker.clone();
                            let ind  = c.industry.clone();
                            let pe_s = format!("{:.1}", c.pe);
                            let ipe  = format!("{:.1}", c.industry_median_pe);
                            let rpe  = format!("{:.2}", c.relative_pe);
                            view! {
                                <tr>
                                    <td style="padding:11px 6px;">
                                        <input type="checkbox"
                                            style="width:16px;height:16px;accent-color:var(--accent);"
                                            prop:checked=move || selected.with(|s| s.contains(&t1))
                                            on:change=move |_| selected.update(|s| {
                                                if !s.remove(&t2) { s.insert(t2.clone()); }
                                            }) />
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
                            <div style="display:flex;flex-direction:column;gap:18px;">
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
                                                <tr style="border-bottom:2px solid var(--border-soft);">
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
                                                gap:16px;margin-top:18px;flex-wrap:wrap;">
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
                                                            justify-content:center;gap:12px;">
                                                    <BdButton variant="dark".to_string() size="sm".to_string()
                                                        disabled=n_dis
                                                        on_click=Box::new(move || run_selected_k(pe_index.get().saturating_sub(1)))>
                                                        "\u{25c4} Newer"
                                                    </BdButton>
                                                    <span style="font-family:var(--font-mono);font-weight:700;\
                                                                 font-size:14px;color:var(--text-strong);\
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
                                            <p class="bd-overline" style="margin:0 0 12px;letter-spacing:var(--tracking-overline);">
                                                "P/E over time \u{2014} dots are troughs, red is your entry"
                                            </p>
                                            <div style="display:grid;\
                                                        grid-template-columns:repeat(auto-fill,minmax(200px,1fr));\
                                                        gap:12px;">{charts}</div>
                                            <p style="font-size:12px;color:var(--text-muted);margin:10px 0 0;text-align:center;">
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
                        <div style="text-align:center;padding:var(--space-7) 0;color:var(--text-muted);">
                            <div style="font-size:var(--text-title);margin-bottom:var(--space-2);">"⏱"</div>
                            "Define a strategy and run."
                        </div>
                    }.into_view(),
                }
            }}
        </main>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount_to_body(App);
}
