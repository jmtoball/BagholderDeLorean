//! One-page strategy dashboard (Leptos CSR). Reuses bagholder-core's DTOs so
//! the JSON from the API deserializes straight into typed structs. Equity
//! curve is an inline SVG polyline — no charting dependency.

use bagholder_core::BacktestResult;
use leptos::*;

const CHART_W: f64 = 720.0;
const CHART_H: f64 = 240.0;

#[component]
fn App() -> impl IntoView {
    let (ticker, set_ticker) = create_signal("AAPL".to_string());
    let (strategy, set_strategy) = create_signal("buy_and_hold".to_string());
    let (fast, set_fast) = create_signal(20usize);
    let (slow, set_slow) = create_signal(50usize);
    let (result, set_result) = create_signal::<Option<Result<BacktestResult, String>>>(None);
    let (loading, set_loading) = create_signal(false);

    let run = move |_| {
        let url = format!(
            "/api/backtest?ticker={}&strategy={}&fast={}&slow={}",
            ticker.get(),
            strategy.get(),
            fast.get(),
            slow.get()
        );
        set_loading.set(true);
        spawn_local(async move {
            let res = async {
                let resp = gloo_net::http::Request::get(&url)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                if !resp.ok() {
                    return Err(resp.text().await.unwrap_or_default());
                }
                resp.json::<BacktestResult>().await.map_err(|e| e.to_string())
            }
            .await;
            set_result.set(Some(res));
            set_loading.set(false);
        });
    };

    view! {
        <main style="font-family:system-ui;max-width:760px;margin:2rem auto;padding:0 1rem">
            <h1>"Bagholder DeLorean"</h1>
            <div style="display:flex;gap:.75rem;flex-wrap:wrap;align-items:end">
                <label>"Ticker"<br/>
                    <input prop:value=ticker
                        on:input=move |e| set_ticker.set(event_target_value(&e)) />
                </label>
                <label>"Strategy"<br/>
                    <select on:change=move |e| set_strategy.set(event_target_value(&e))>
                        <option value="buy_and_hold">"Buy & Hold"</option>
                        <option value="sma_crossover">"SMA crossover"</option>
                    </select>
                </label>
                <label>"Fast"<br/>
                    <input type="number" prop:value=move || fast.get().to_string()
                        on:input=move |e| set_fast.set(event_target_value(&e).parse().unwrap_or(20)) />
                </label>
                <label>"Slow"<br/>
                    <input type="number" prop:value=move || slow.get().to_string()
                        on:input=move |e| set_slow.set(event_target_value(&e).parse().unwrap_or(50)) />
                </label>
                <button on:click=run prop:disabled=loading>"Run backtest"</button>
            </div>

            {move || match result.get() {
                None => view! { <p>"Define a strategy and run."</p> }.into_view(),
                Some(Err(e)) => view! { <p style="color:#c00">"Error: " {e}</p> }.into_view(),
                Some(Ok(r)) => view! {
                    <section>
                        <ul>
                            <li>"Total return: " {fmt_pct(r.metrics.total_return)}</li>
                            <li>"CAGR: " {fmt_pct(r.metrics.cagr)}</li>
                            <li>"Max drawdown: " {fmt_pct(r.metrics.max_drawdown)}</li>
                            <li>"Sharpe: " {format!("{:.2}", r.metrics.sharpe)}</li>
                        </ul>
                        {equity_svg(&r)}
                    </section>
                }.into_view(),
            }}
        </main>
    }
}

fn fmt_pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

fn equity_svg(r: &BacktestResult) -> View {
    let eq: Vec<f64> = r.curve.iter().map(|p| p.equity).collect();
    if eq.len() < 2 {
        return view! { <p>"Not enough data to chart."</p> }.into_view();
    }
    let (min, max) = eq
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
    let span = (max - min).max(1e-9);
    let points: String = eq
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = i as f64 / (eq.len() - 1) as f64 * CHART_W;
            let y = CHART_H - (v - min) / span * CHART_H;
            format!("{x:.1},{y:.1} ")
        })
        .collect();

    view! {
        <svg viewBox=format!("0 0 {CHART_W} {CHART_H}") preserveAspectRatio="none"
             style="border:1px solid #ddd;width:100%;height:auto">
            <polyline points=points fill="none" stroke="#2563eb" stroke-width="2" />
        </svg>
    }
    .into_view()
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount_to_body(App);
}
