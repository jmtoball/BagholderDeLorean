//! One-page dashboard (Leptos CSR). Two flows behind a category selector:
//!  - Price Strategies: ticker + strategy -> single backtest.
//!  - Fundamentals: run a screen -> pick names -> backtest them overlaid.
//! Reuses bagholder-core's DTOs so API JSON deserializes into typed structs.
//! Charts are inline SVG polylines — no charting dependency.

use std::collections::{HashMap, HashSet};

use bagholder_core::{BacktestResult, Candidate, PeHistory};
use chrono::{Datelike, NaiveDate};
use leptos::*;
use serde::de::DeserializeOwned;

const PALETTE: &[&str] = &[
    "#2563eb", "#dc2626", "#16a34a", "#d97706", "#7c3aed", "#0891b2", "#db2777", "#65a30d",
];
const CHART_W: f64 = 720.0;
const CHART_H: f64 = 260.0;

async fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    let resp = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err(resp.text().await.unwrap_or_default());
    }
    resp.json::<T>().await.map_err(|e| e.to_string())
}

fn fmt_pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

/// Overlay one or more equity curves on a shared date/value scale, with a
/// legend showing each series' total return. Curves all start at 1.0, so they
/// are directly comparable.
fn equity_overlay(series: &[(String, BacktestResult)]) -> View {
    let series: Vec<&(String, BacktestResult)> =
        series.iter().filter(|(_, r)| r.curve.len() >= 2).collect();
    if series.is_empty() {
        return view! { <p>"Not enough data to chart."</p> }.into_view();
    }

    let (mut dmin, mut dmax) = (i32::MAX, i32::MIN);
    let (mut ymin, mut ymax) = (f64::MAX, f64::MIN);
    for (_, r) in &series {
        for p in &r.curve {
            let d = p.date.num_days_from_ce();
            dmin = dmin.min(d);
            dmax = dmax.max(d);
            ymin = ymin.min(p.equity);
            ymax = ymax.max(p.equity);
        }
    }
    let dspan = (dmax - dmin).max(1) as f64;
    let yspan = (ymax - ymin).max(1e-9);

    let lines = series
        .iter()
        .enumerate()
        .map(|(i, (_, r))| {
            let color = PALETTE[i % PALETTE.len()];
            let points: String = r
                .curve
                .iter()
                .map(|p| {
                    let x = (p.date.num_days_from_ce() - dmin) as f64 / dspan * CHART_W;
                    let y = CHART_H - (p.equity - ymin) / yspan * CHART_H;
                    format!("{x:.1},{y:.1} ")
                })
                .collect();
            view! { <polyline points=points fill="none" stroke=color stroke-width="1.5" /> }
        })
        .collect_view();

    let legend = series
        .iter()
        .enumerate()
        .map(|(i, (name, r))| {
            let color = PALETTE[i % PALETTE.len()];
            let swatch = format!(
                "display:inline-block;width:10px;height:10px;background:{color};margin-right:4px"
            );
            // Show the entry date + P/E when this run used a P/E-minimum entry.
            let entry = match (r.entry_date, r.entry_pe) {
                (Some(d), Some(pe)) => format!(" (from {d}, P/E {pe:.1})"),
                _ => String::new(),
            };
            view! {
                <span style="margin-right:1rem;white-space:nowrap">
                    <span style=swatch></span>
                    {name.clone()} " " {fmt_pct(r.metrics.total_return)} {entry}
                </span>
            }
        })
        .collect_view();

    view! {
        <div>
            <svg viewBox=format!("0 0 {CHART_W} {CHART_H}") preserveAspectRatio="none"
                 style="border:1px solid #ddd;width:100%;height:auto">
                {lines}
            </svg>
            <div style="margin-top:.5rem;font-size:.9rem">{legend}</div>
        </div>
    }
    .into_view()
}

/// A compact P/E-over-time chart: the series as a line, troughs as dots, and
/// the current entry trough highlighted in red. Low P/E sits at the bottom.
fn pe_chart(ticker: &str, h: &PeHistory, entry: Option<NaiveDate>) -> View {
    if h.series.len() < 2 {
        return view! { <p style="font-size:.85rem;color:#999">{ticker.to_string()}": no P/E history"</p> }.into_view();
    }
    let (w, ht) = (720.0, 120.0);
    let (mut dmin, mut dmax) = (i32::MAX, i32::MIN);
    let (mut pmin, mut pmax) = (f64::MAX, f64::MIN);
    for p in &h.series {
        let d = p.date.num_days_from_ce();
        dmin = dmin.min(d);
        dmax = dmax.max(d);
        pmin = pmin.min(p.pe);
        pmax = pmax.max(p.pe);
    }
    let dspan = (dmax - dmin).max(1) as f64;
    let pspan = (pmax - pmin).max(1e-9);
    let xy = move |date: NaiveDate, pe: f64| {
        let x = (date.num_days_from_ce() - dmin) as f64 / dspan * w;
        let y = ht - (pe - pmin) / pspan * ht; // low P/E -> bottom
        (x, y)
    };

    let line: String = h
        .series
        .iter()
        .map(|p| {
            let (x, y) = xy(p.date, p.pe);
            format!("{x:.1},{y:.1} ")
        })
        .collect();
    let dots = h
        .troughs
        .iter()
        .map(|t| {
            let (x, y) = xy(t.date, t.pe);
            let (r, fill) = if entry == Some(t.date) {
                ("4", "#dc2626")
            } else {
                ("2.5", "#888")
            };
            view! { <circle cx=format!("{x:.1}") cy=format!("{y:.1}") r=r fill=fill /> }
        })
        .collect_view();

    view! {
        <div style="margin-bottom:.75rem">
            <div style="font-size:.85rem;font-weight:600">
                {ticker.to_string()}" — P/E "{format!("(range {pmin:.1}–{pmax:.1})")}
            </div>
            <svg viewBox=format!("0 0 {w} {ht}") preserveAspectRatio="none"
                 style="border:1px solid #eee;width:100%;height:80px">
                <polyline points=line fill="none" stroke="#555" stroke-width="1" />
                {dots}
            </svg>
        </div>
    }
    .into_view()
}

#[component]
fn App() -> impl IntoView {
    let category = create_rw_signal("price".to_string());
    let years = create_rw_signal(10u32);
    let busy = create_rw_signal(false);

    // Price-strategy branch
    let ticker = create_rw_signal("AAPL".to_string());
    let price_strategy = create_rw_signal("buy_and_hold".to_string());
    let fast = create_rw_signal(20usize);
    let slow = create_rw_signal(50usize);
    let price_result = create_rw_signal::<Option<Result<BacktestResult, String>>>(None);

    // Fundamentals branch
    let candidates = create_rw_signal::<Option<Result<Vec<Candidate>, String>>>(None);
    let selected = create_rw_signal::<HashSet<String>>(HashSet::new());
    let overlay = create_rw_signal::<Vec<(String, BacktestResult)>>(Vec::new());
    let pe_entry = create_rw_signal(false); // enter at each name's local-min P/E
    let pe_index = create_rw_signal(0usize); // which trough, 0 = most recent
    let pe_hist = create_rw_signal::<HashMap<String, PeHistory>>(HashMap::new()); // per-ticker, cached

    let run_price = move |_| {
        let url = format!(
            "/api/backtest?ticker={}&strategy={}&fast={}&slow={}&years={}",
            ticker.get(),
            price_strategy.get(),
            fast.get(),
            slow.get(),
            years.get()
        );
        busy.set(true);
        spawn_local(async move {
            price_result.set(Some(get_json::<BacktestResult>(&url).await));
            busy.set(false);
        });
    };

    let run_screen = move |_| {
        busy.set(true);
        selected.update(|s| s.clear());
        overlay.set(Vec::new());
        pe_hist.update(|m| m.clear());
        spawn_local(async move {
            let res = get_json::<Vec<Candidate>>("/api/screen?kind=low_pe&limit=12").await;
            candidates.set(Some(res));
            busy.set(false);
        });
    };

    // Backtest the selected names. With pe_entry on, enter at each name's k-th
    // trough (0 = most recent); otherwise use the fixed timeframe.
    let run_selected_k = move |k: usize| {
        let tickers: Vec<String> = selected.get().into_iter().collect();
        let use_pe = pe_entry.get();
        let entry = if use_pe {
            format!("&entry=pe_min&pe_index={k}")
        } else {
            format!("&years={}", years.get())
        };
        // P/E history is identical across steps — fetch each ticker's once.
        let cached: HashSet<String> = pe_hist.get().keys().cloned().collect();
        pe_index.set(k);
        busy.set(true);
        spawn_local(async move {
            let mut out = Vec::new();
            let mut new_hist: Vec<(String, PeHistory)> = Vec::new();
            for t in tickers {
                let url = format!("/api/backtest?ticker={t}&strategy=buy_and_hold{entry}");
                if let Ok(r) = get_json::<BacktestResult>(&url).await {
                    out.push((t.clone(), r));
                }
                if use_pe && !cached.contains(&t) {
                    if let Ok(h) = get_json::<PeHistory>(&format!("/api/pe_history?ticker={t}")).await {
                        new_hist.push((t, h));
                    }
                }
            }
            if !new_hist.is_empty() {
                pe_hist.update(|m| m.extend(new_hist));
            }
            overlay.set(out);
            busy.set(false);
        });
    };

    view! {
        <main style="font-family:system-ui;max-width:820px;margin:2rem auto;padding:0 1rem">
            <h1>"Bagholder DeLorean"</h1>

            <div style="display:flex;gap:.75rem;flex-wrap:wrap;align-items:end">
                <label>"Category"<br/>
                    <select on:change=move |e| category.set(event_target_value(&e))>
                        <option value="price">"Price Strategies"</option>
                        <option value="fundamentals">"Fundamentals"</option>
                    </select>
                </label>

                // Second-level choice depends on category
                {move || if category.get() == "price" {
                    view! {
                        <label>"Strategy"<br/>
                            <select on:change=move |e| price_strategy.set(event_target_value(&e))>
                                <option value="buy_and_hold">"Buy & Hold"</option>
                                <option value="sma_crossover">"SMA crossover"</option>
                            </select>
                        </label>
                        <label>"Ticker"<br/>
                            <input prop:value=ticker on:input=move |e| ticker.set(event_target_value(&e)) />
                        </label>
                    }.into_view()
                } else {
                    view! {
                        <label>"Screen"<br/>
                            <select>
                                <option value="low_pe">"Low P/E (industry-rel.)"</option>
                            </select>
                        </label>
                    }.into_view()
                }}

                // SMA params, only when relevant
                {move || (category.get() == "price" && price_strategy.get() == "sma_crossover")
                    .then(|| view! {
                        <label>"Fast"<br/>
                            <input type="number" prop:value=move || fast.get().to_string()
                                on:input=move |e| fast.set(event_target_value(&e).parse().unwrap_or(20)) />
                        </label>
                        <label>"Slow"<br/>
                            <input type="number" prop:value=move || slow.get().to_string()
                                on:input=move |e| slow.set(event_target_value(&e).parse().unwrap_or(50)) />
                        </label>
                    })}

                <label>"Timeframe"<br/>
                    <select on:change=move |e| years.set(event_target_value(&e).parse().unwrap_or(10))>
                        <option value="1">"1y"</option>
                        <option value="3">"3y"</option>
                        <option value="5">"5y"</option>
                        <option value="10" selected=true>"10y"</option>
                        <option value="0">"Max"</option>
                    </select>
                </label>

                {move || if category.get() == "price" {
                    view! { <button on:click=run_price prop:disabled=move || busy.get()>"Run backtest"</button> }.into_view()
                } else {
                    view! { <button on:click=run_screen prop:disabled=move || busy.get()>"Run screen"</button> }.into_view()
                }}
            </div>

            // --- Price results ---
            {move || (category.get() == "price").then(|| view! {
                <section>
                    {move || match price_result.get() {
                        None => view! { <p>"Define a strategy and run."</p> }.into_view(),
                        Some(Err(e)) => view! { <p style="color:#c00">"Error: " {e}</p> }.into_view(),
                        Some(Ok(r)) => {
                            let series = vec![(ticker.get(), r.clone())];
                            view! {
                                <ul>
                                    <li>"Total return: " {fmt_pct(r.metrics.total_return)}</li>
                                    <li>"CAGR: " {fmt_pct(r.metrics.cagr)}</li>
                                    <li>"Max drawdown: " {fmt_pct(r.metrics.max_drawdown)}</li>
                                    <li>"Sharpe: " {format!("{:.2}", r.metrics.sharpe)}</li>
                                </ul>
                                {equity_overlay(&series)}
                            }.into_view()
                        }
                    }}
                </section>
            })}

            // --- Fundamentals: screen table + overlay ---
            {move || (category.get() == "fundamentals").then(|| view! {
                <section>
                    {move || match candidates.get() {
                        None => view! { <p>"Run the screen to surface cheap-vs-industry names."</p> }.into_view(),
                        Some(Err(e)) => view! { <p style="color:#c00">"Error: " {e}</p> }.into_view(),
                        Some(Ok(cands)) => {
                            let rows = cands.iter().map(|c| {
                                let t_checked = c.ticker.clone();
                                let t_toggle = c.ticker.clone();
                                view! {
                                    <tr>
                                        <td>
                                            <input type="checkbox"
                                                prop:checked=move || selected.with(|s| s.contains(&t_checked))
                                                on:change=move |_| selected.update(|s| {
                                                    if !s.remove(&t_toggle) { s.insert(t_toggle.clone()); }
                                                }) />
                                        </td>
                                        <td>{c.ticker.clone()}</td>
                                        <td>{c.industry.clone()}</td>
                                        <td style="text-align:right">{format!("{:.1}", c.pe)}</td>
                                        <td style="text-align:right">{format!("{:.1}", c.industry_median_pe)}</td>
                                        <td style="text-align:right">{format!("{:.2}", c.relative_pe)}</td>
                                    </tr>
                                }
                            }).collect_view();
                            view! {
                                <table style="border-collapse:collapse;width:100%;font-size:.9rem">
                                    <thead>
                                        <tr style="text-align:left;border-bottom:1px solid #ccc">
                                            <th></th><th>"Ticker"</th><th>"Industry"</th>
                                            <th style="text-align:right">"P/E"</th>
                                            <th style="text-align:right">"Ind. median"</th>
                                            <th style="text-align:right">"Rel."</th>
                                        </tr>
                                    </thead>
                                    <tbody>{rows}</tbody>
                                </table>
                                <div style="margin-top:.5rem;display:flex;gap:.75rem;align-items:center">
                                    <button on:click=move |_| run_selected_k(0) prop:disabled=move || busy.get()>"Backtest selected"</button>
                                    <label title="Start each backtest at a local-minimum P/E instead of the fixed timeframe; step through troughs below">
                                        <input type="checkbox" prop:checked=pe_entry
                                            on:change=move |e| pe_entry.set(event_target_checked(&e)) />
                                        " enter at local-min P/E"
                                    </label>
                                </div>
                            }.into_view()
                        }
                    }}
                    // Step through troughs: ◀ newer / older ▶ (only in pe_min mode)
                    {move || {
                        let maxn = overlay.get().iter().filter_map(|(_, r)| r.entry_count).max();
                        match (pe_entry.get(), maxn) {
                            (true, Some(n)) if n > 1 => {
                                let k = pe_index.get();
                                Some(view! {
                                    <div style="margin-top:.5rem;display:flex;gap:.5rem;align-items:center">
                                        <button prop:disabled=move || busy.get() || pe_index.get() == 0
                                            on:click=move |_| run_selected_k(pe_index.get().saturating_sub(1))>"◀ newer"</button>
                                        <span>{format!("trough {} of {} (0 = most recent)", k + 1, n)}</span>
                                        <button prop:disabled=move || busy.get() || (pe_index.get() + 1 >= n)
                                            on:click=move |_| run_selected_k(pe_index.get() + 1)>"older ▶"</button>
                                    </div>
                                })
                            }
                            _ => None,
                        }
                    }}
                    {move || { let o = overlay.get(); (!o.is_empty()).then(|| equity_overlay(&o)) }}

                    // P/E-over-time charts with troughs marked (pe_min mode only)
                    {move || {
                        let results = overlay.get();
                        let hist = pe_hist.get();
                        (pe_entry.get() && !results.is_empty()).then(|| {
                            let charts = results
                                .iter()
                                .filter_map(|(t, r)| hist.get(t).map(|h| pe_chart(t, h, r.entry_date)))
                                .collect_view();
                            view! {
                                <div style="margin-top:1rem">
                                    <div style="font-size:.85rem;color:#666;margin-bottom:.4rem">
                                        "P/E over time — dots are troughs, red is the current entry (lower = cheaper)"
                                    </div>
                                    {charts}
                                </div>
                            }
                        })
                    }}
                </section>
            })}
        </main>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount_to_body(App);
}
