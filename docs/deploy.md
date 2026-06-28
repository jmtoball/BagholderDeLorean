# Deployment

## Chosen platform: Fly.io

**Why Fly.io**: single-binary Rust app, persistent volume for DuckDB, outbound HTTPS included, no
cold-start penalty on the shared-1x machine tier. Docker layer caching keeps redeployment fast
after the first (slow) DuckDB C++ build.

### Monthly cost estimate

| Resource              | Cost     |
|-----------------------|----------|
| shared-cpu-1x 512 MB  | ~$3.83   |
| 1 GB persistent volume| $0.15    |
| Egress (Yahoo/SEC)    | free     |
| **Total**             | **~$4/mo** |

Scale up to 1 GB RAM ($7.66/mo) if DuckDB query concurrency becomes a bottleneck.

### Blockers identified

- **DuckDB build time**: the bundled C++ lib compiles in ~8–15 min on a fresh Docker layer.
  Solution: Docker multi-stage build with a pre-built `cargo chef` layer to cache deps separately.
- **Binary size**: release binary is ~150 MB unstripped. Add `strip = true` in `[profile.release]`
  to bring it to ~60–80 MB. The WASM blob (`crates/web/dist`) adds another ~4–6 MB.
- **DuckDB path**: currently hardcoded to `"bagholder.duckdb"` in `crates/api/src/main.rs:463`.
  Need to read from `DATA_DIR` env var (default `.`) so Fly mounts at `/data`.

### Deployment steps (once wiring issues are closed)

```bash
# 1. Install flyctl and authenticate
brew install flyctl && fly auth login

# 2. Launch the app (creates fly.toml and provisions the VM)
fly launch --name bagholder --region fra --no-deploy

# 3. Create a persistent volume for DuckDB
fly volumes create bagholder_data --size 1 --region fra

# 4. Deploy
fly deploy

# 5. Set optional env vars
fly secrets set SEC_UA="bagholder-delorean/1.0 you@example.com"
fly secrets set POLYGON_API_KEY="..."
```

### Implementation issues spawned

- #38 — Add `DATA_DIR` env var to Store::open path in API + `Dockerfile` + `fly.toml`
