# ── Stage 1: chef planner — capture the dependency graph ──────────────────────
FROM rust:1-slim-bookworm AS chef
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked

WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 2: cargo-chef build — compiles all deps (cached layer) ──────────────
FROM chef AS builder-deps
COPY --from=chef /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# ── Stage 3: WASM frontend ────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS wasm-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN rustup target add wasm32-unknown-unknown
# ponytail: pin trunk version for reproducible builds
RUN cargo install trunk --locked --version 0.20.3

WORKDIR /app
COPY . .
WORKDIR /app/crates/web
RUN trunk build --release

# ── Stage 4: API binary ───────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS api-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder-deps /usr/local/cargo /usr/local/cargo
COPY --from=builder-deps /app/target target
COPY . .
# Copy built frontend assets so they get embedded
COPY --from=wasm-builder /app/crates/web/dist crates/web/dist

RUN cargo build --release -p bagholder-api

# ── Stage 5: runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=api-builder /app/target/release/bagholder-api /usr/local/bin/bagholder-api

ENV PORT=3000
EXPOSE 3000

CMD ["bagholder-api"]
