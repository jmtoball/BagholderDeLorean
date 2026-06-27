//! HTTP API: runs backtests and serves the WASM frontend.
//!   GET /api/backtest?ticker=AAPL.US&strategy=sma_crossover&fast=20&slow=50

use std::sync::{Arc, Mutex};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use bagholder_core::{run_backtest, BacktestResult, Strategy};
use bagholder_data::Store;
use serde::Deserialize;
use tower_http::{cors::CorsLayer, services::ServeDir};

// ponytail: one global lock around the DuckDB connection — fine for single-user
// dev. Swap for a connection pool if concurrent throughput ever matters.
type Db = Arc<Mutex<Store>>;

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
    State(db): State<Db>,
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
    // Blocking DB + network I/O must not run on the async runtime's workers.
    let bars = tokio::task::spawn_blocking(move || db.lock().unwrap().ohlcv(&ticker))
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
    let db: Db = Arc::new(Mutex::new(
        Store::open("bagholder.duckdb").expect("opening data store"),
    ));

    let app = Router::new()
        .route("/api/backtest", get(backtest))
        // Serve the trunk-built frontend. Run `trunk build` in crates/web first.
        .fallback_service(ServeDir::new("crates/web/dist"))
        .layer(CorsLayer::permissive())
        .with_state(db);

    let addr = "127.0.0.1:3000";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}
