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

# Browser smoke test (Playwright) — drives both UI flows, screenshots to e2e/shots/
# With the app running on :3000 (see crates/web/e2e/README.md):
cd crates/web/e2e && npm link playwright && node validate.mjs
```

For the full stack locally: `cargo run -p bagholder-api` in one terminal, `trunk serve` (from `crates/web`) in another, open http://localhost:8080.

## Architecture

Four crates, dependency-ordered so the compute core stays portable:

- **`core`** (`bagholder-core`) — the backtest engine. Pure compute, no I/O. Deps limited to `serde`/`chrono` *on purpose*: it compiles to WASM so the `web` crate reuses its DTOs (`Bar`, `BacktestResult`, `Strategy`), making API responses deserialize into the same typed structs the engine produced. **Do not add I/O or native-only deps here** — it would break the wasm build of `web`.
- **`data`** (`bagholder-data`) — historic data clients + a **DuckDB cache** (`Store`, `crates/data/src/store.rs`) + **fundamental screeners** (`screen.rs`). `Store::ohlcv(ticker)` serves cached bars or downloads from **Yahoo Finance's v8 chart API** (`download_ohlcv`, free, no key). **Tickers are plain Yahoo symbols** — `AAPL`, `BRK-B` (no exchange suffix); this matches SEC's spelling too. Close is split/dividend-adjusted (adjclose); open/high/low are raw. (Note: Stooq was the original source but now gates its CSV behind a JS proof-of-work challenge — don't go back to it.) DuckDB is embedded (single `bagholder.duckdb` file) and columnar — fast range scans for backtests, SQL joins for fundamentals later. Schema: `bars` is wide (`ticker,date,o,h,l,c,v`); `fundamentals` is tall (`ticker,period,metric,period_type,value`) so the metric set stays open-ended. **Fundamentals** come from SEC EDGAR: `Store::fundamentals(ticker)` resolves the ticker→CIK via SEC's `company_tickers.json` (cached in the `cik_map` table), then pulls `companyfacts` and extracts a curated metric set (`METRICS` in `lib.rs`: revenue, net_income, eps_basic, assets, liabilities, equity, shares_outstanding). XBRL income facts are durations, so each is classified `Q` or `FY` by period length (`classify_period`) and YTD cumulatives are dropped — `period_type` is part of the PK so a quarter's revenue isn't conflated with the year's. SEC requires a contact User-Agent (`SEC_UA`). **First build is slow** (DuckDB compiles a bundled C++ lib). Depends on `core` for `Bar`. **Never make this a dependency of `web`** (pulls native TLS + DuckDB).
- **`api`** (`bagholder-api`) — axum server. Holds the `Store` behind `Arc<Mutex<>>` (one global lock — fine for single-user dev). Endpoints: `GET /api/backtest` (load cached data + run engine; `entry=pe_min` enters at a local-minimum P/E instead of a fixed `years` window — uses `core::pe_series` + `core::local_minima`; `pe_index` steps back through troughs (0 = most recent, clamped), and the result carries `entry_date`/`entry_pe`/`entry_index`/`entry_count` so the UI can show "trough k of N" and step), `GET /api/fundamentals?ticker=`, `GET /api/pe_history?ticker=` (P/E series + trough points via `core::pe_history`, for the per-name P/E chart that marks troughs and highlights the current entry), `GET /api/screen?kind=low_pe&limit=N` (rank `DEFAULT_UNIVERSE` by industry-relative P/E — **cold call warms ~23 names, ~2 min; cached after**). Also serves `crates/web/dist`. Blocking DB/network calls go through `tokio::task::spawn_blocking` — keep them off the async runtime's worker threads.
- **`web`** (`bagholder-web`) — Leptos CSR one-pager. Form → fetch `/api/backtest` → metrics + inline-SVG equity curve. No charting dependency by design.

Data flow: `web` form → `api/backtest` → `data::Store::ohlcv` (DuckDB cache → Stooq on miss) → `core::run_backtest` → JSON `BacktestResult` → `web` renders.

### Conventions that matter

- **No lookahead in the engine.** Signals at bar `i` use only data through bar `i`; `run_backtest` applies *yesterday's* signal to *today's* return. Any new strategy must preserve this — the `sma_has_no_lookahead` test guards it. (Exception by design: `local_minima`/the `pe_min` entry are *retrospective* selection tools — they inspect later data to confirm a trough, then the backtest from that date shows forward performance. That's intended, not a leak.)
- **Strategies are an enum** (`core::Strategy`), not a trait, so the web form serializes a choice directly. Add a variant + a match arm in `signals()` to add a strategy. Move to a trait only when users need custom plug-ins.
- **`ponytail:` comments** mark deliberate shortcuts with their upgrade path (e.g. the fundamentals stub, enum-vs-trait). Honor them; don't "fix" a marked simplification without reason.
- Metrics (`compute_metrics`) assume daily bars and ~252 trading days/year for CAGR/Sharpe annualization.
- **Design prototypes are high-fidelity, not inspiration.** The React files under `design_system/` (gitignored, resynced from the design source — never edit them or store anything you need to keep there) are *the design*. When building a web surface, port the matching prototype faithfully — layout, affordances, states, copy, sub-components. Missing a component → implement it (port the React piece to an inline-SVG Leptos one); used more than once → make it reusable in `crates/web/src/components.rs`, not copy-pasted. Verify every state with Playwright screenshots (`crates/web/e2e/`) against the prototype — a compile check or passing acceptance text is not "done", the screenshot is. If you must ship less than the design, say so explicitly and open a follow-up; silently shipping "functional but plainer" is a regression.
