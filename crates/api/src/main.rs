//! HTTP API: runs backtests and serves the WASM frontend.
//!   GET /api/backtest?ticker=AAPL.US&strategy=sma_crossover&fast=20&slow=50

use axum::{extract::Query, http::StatusCode, routing::get, Json, Router};
use bagholder_core::{run_backtest, BacktestResult, Strategy};
use serde::Deserialize;
use tower_http::{cors::CorsLayer, services::ServeDir};

#[derive(Deserialize)]
struct BacktestQuery {
    ticker: String,
    #[serde(default = "default_strategy")]
    strategy: String,
    fast: Option<usize>,
    slow: Option<usize>,
}

fn default_strategy() -> String {
    "buy_and_hold".into()
}

async fn backtest(
    Query(q): Query<BacktestQuery>,
) -> Result<Json<BacktestResult>, (StatusCode, String)> {
    let strategy = match q.strategy.as_str() {
        "sma_crossover" => Strategy::SmaCrossover {
            fast: q.fast.unwrap_or(20),
            slow: q.slow.unwrap_or(50),
        },
        _ => Strategy::BuyAndHold,
    };

    let ticker = q.ticker.clone();
    // Blocking reqwest must not run on the async runtime's worker threads.
    let bars = tokio::task::spawn_blocking(move || bagholder_data::fetch_ohlcv(&ticker))
        .await
        .map_err(internal)?
        .map_err(internal)?;

    Ok(Json(run_backtest(&bars, &strategy)))
}

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/backtest", get(backtest))
        // Serve the trunk-built frontend. Run `trunk build` in crates/web first.
        .fallback_service(ServeDir::new("crates/web/dist"))
        .layer(CorsLayer::permissive());

    let addr = "127.0.0.1:3000";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}
