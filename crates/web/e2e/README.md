# Browser smoke test

`validate.mjs` drives a real Chromium through the app and screenshots each
state into `shots/` (gitignored). It's a smoke test, not a full suite — extend
it as features land.

## One-time

```bash
npm install -g playwright   # the CLI + library
playwright install chromium # the browser binary
```

## Run

Start the app first (serves API + frontend on :3000):

```bash
trunk build -d crates/web/dist crates/web/index.html   # or: cd crates/web && trunk build
cargo run -p bagholder-api
```

Then, from this directory:

```bash
npm link playwright   # symlink the global package so the ESM import resolves
                      # (NODE_PATH does not work for ESM)
node validate.mjs     # BASE=http://host:port to override the URL
```

Exit code is non-zero and the last line reads `VALIDATION FAILED` if anything
breaks. The first fundamentals run warms ~23 names server-side (a few minutes);
subsequent runs are fast.
