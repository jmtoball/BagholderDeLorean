# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Backtesting app for trading strategies. Rust workspace, end-to-end (the frontend is WASM, not JS).

## Commands

```bash
# Tests / checks (run the engine self-checks while iterating on strategies)
cargo test -p bagholder-core            # backtest engine unit tests
cargo test -p bagholder-data            # CSV parsing tests (no network)
cargo test -p bagholder-core sma_has_no_lookahead   # a single test by name

# Run the API server (serves backtests + the built frontend on :3000)
cargo run -p bagholder-api

# Frontend (WASM) — needs one-time setup:
rustup target add wasm32-unknown-unknown
cargo install trunk
trunk serve   # dev server on :8080, proxies /api -> :3000 (run from crates/web)
trunk build   # outputs crates/web/dist, which the API server serves in prod

cargo check -p bagholder-web --target wasm32-unknown-unknown   # type-check web without trunk
```

For the full stack locally: `cargo run -p bagholder-api` in one terminal, `trunk serve` (from `crates/web`) in another, open http://localhost:8080.

## Architecture

Four crates, dependency-ordered so the compute core stays portable:

- **`core`** (`bagholder-core`) — the backtest engine. Pure compute, no I/O. Deps limited to `serde`/`chrono` *on purpose*: it compiles to WASM so the `web` crate reuses its DTOs (`Bar`, `BacktestResult`, `Strategy`), making API responses deserialize into the same typed structs the engine produced. **Do not add I/O or native-only deps here** — it would break the wasm build of `web`.
- **`data`** (`bagholder-data`) — historic data clients + a **DuckDB cache** (`Store`, `crates/data/src/store.rs`). `Store::ohlcv(ticker)` serves cached bars or downloads from Stooq (`download_ohlcv`, free, no key, ticker format `AAPL.US`) and caches them. DuckDB is embedded (single `bagholder.duckdb` file) and columnar — fast range scans for backtests, SQL joins for fundamentals later. Schema: `bars` is wide (`ticker,date,o,h,l,c,v`); `fundamentals` is tall (`ticker,period,metric,value`) so the metric set stays open-ended. Fundamentals fetching is still a stub (`fetch_fundamentals`) — wire to SEC EDGAR or Yahoo quoteSummary. **First build is slow** (DuckDB compiles a bundled C++ lib). Depends on `core` for `Bar`. **Never make this a dependency of `web`** (pulls native TLS + DuckDB).
- **`api`** (`bagholder-api`) — axum server. Holds the `Store` behind `Arc<Mutex<>>` (one global lock — fine for single-user dev). `GET /api/backtest` loads cached data + runs the engine; also serves `crates/web/dist`. Blocking DB/network calls go through `tokio::task::spawn_blocking` — keep them off the async runtime's worker threads.
- **`web`** (`bagholder-web`) — Leptos CSR one-pager. Form → fetch `/api/backtest` → metrics + inline-SVG equity curve. No charting dependency by design.

Data flow: `web` form → `api/backtest` → `data::Store::ohlcv` (DuckDB cache → Stooq on miss) → `core::run_backtest` → JSON `BacktestResult` → `web` renders.

### Conventions that matter

- **No lookahead in the engine.** Signals at bar `i` use only data through bar `i`; `run_backtest` applies *yesterday's* signal to *today's* return. Any new strategy must preserve this — the `sma_has_no_lookahead` test guards it.
- **Strategies are an enum** (`core::Strategy`), not a trait, so the web form serializes a choice directly. Add a variant + a match arm in `signals()` to add a strategy. Move to a trait only when users need custom plug-ins.
- **`ponytail:` comments** mark deliberate shortcuts with their upgrade path (e.g. the fundamentals stub, enum-vs-trait). Honor them; don't "fix" a marked simplification without reason.
- Metrics (`compute_metrics`) assume daily bars and ~252 trading days/year for CAGR/Sharpe annualization.
