# Remote-host feasibility lab — stage 1 complete

**Selected: TypeScript Worker + TypeScript SQLite Durable Objects + per-DO,
disposable synchronous Rust Wasm instances.** The complete local workerd ledger
passes. See [evidence.md](evidence.md) and [observations.json](observations.json).
Stop for review before stage 2; the remote product is not implemented.

This lab is independent of production `src/` and the root Cargo manifest/lockfile.
It contains synthetic SQL and Rust/Jiff capability expressions, not copied billing
rules or an extracted product core. TS owns platform lifecycle and transactions;
Rust owns synthetic data SQL and exact arithmetic. The optional MCP adapter uses
the established SDK over an authenticated private binding, with no domain rules.

## Safety

**Never deploy this lab:** its credentials are deliberately public test fixtures.
No accounts, management tokens, real tracking data, remote bindings, provisioning,
login or deployment are needed. Vite disables remote bindings/tunnels and binds
loopback port 8799. Tests run actual workerd with emulated SQLite/R2 in temporary
persistence directories. Only external I/O timing is controlled by `LAB_GATE`.
No SDK fork, generated-glue patch, alternate database or handwritten MCP protocol.

`fallback` is the original experiment's directory/mode name, not a product fallback
from unavailable remote service to local tracking. Bootstrap schemas, receipt keys
and role fixtures are lab-only, not production routing/migration/protocol designs.

## Clean-checkout reproduction

Prerequisites: Node **24.21.0**, npm **11.19.0**, rustup and network access to obtain
pinned tools. Tested on Apple M5 Max / arm64 macOS, Darwin 25.6.0.

```sh
cd spikes/remote-host
rustup toolchain install 1.98.1 --profile minimal --component rustfmt,clippy \
  --target wasm32-unknown-unknown
npm ci
npm run build                 # selected Rust capability core -> cf -> Vite
npm run check:js
npm test                      # full selected-host ledger, about 30 seconds
mkdir -p results
npm run check:startup > results/fallback-startup.json
./node_modules/.bin/cf workers check --prebuilt --mode fallback --worker read-mcp \
  --outfile results/mcp-startup.cpuprofile > results/mcp-startup.json
```

`npm test` loads the **same cf-built JS/Wasm** for auth, MCP, exact SQL, transactions,
lifetime, eviction/restart, R2/alarms and costs. It writes `results/fallback.json`;
ignored profiles/artifacts are regenerated, while curated observations are checked
in. Test failures and timeouts fail the suite. Persistence directories are removed.

`cf build` does not compile Rust itself: use the scripts. Builds overwrite ignored
`.cloudflare/output`, so rebuild the selected shape after running the repro below.
Do not substitute Wrangler. Mode `local` is reserved by Vite; `fallback` is local-only
through configuration, not through its name. `npm run dev` serves named probes:

```sh
curl -H 'Authorization: Bearer lab-write-only' \
  http://127.0.0.1:8799/workspace/synthetic/exact
# Also: state, commit, rollback-err, rollback-js, negative-control, trap,
# constraint?kind=0, bad-numeric, seed?rows=256, indexed, calendar, capture,
# backup?job=one, schedule?job=one, alarm-state.
```

## Minimal rejected-host repro (optional)

The losing Rust capability harness was removed. `src/` now contains only a small,
useful workers-rs shared-runtime trap repro; it is not a second maintained host.
Its public write-token equality fixture is **not** the selected host's authentication.

```sh
cargo +1.98.1 install worker-build --version 0.8.6 --locked --root .tools
export PATH="$PWD/.tools/bin:$PATH"
npm run build:rust-repro
npm run test:rust-repro        # EXPECT exit 1: both two-second lifetime gates fail
npm run build                 # restore selected-host artifact before npm test
```

`gate-test.mjs` holds a real Rust SDK future in B while A traps. Both requests remain
unsettled in replacement-first and callback-first orderings, though new requests
work and SQL rolls back. A timeout is a failure, not recovery. The selected suite
requires settled responses within three seconds, independent sibling instances,
and rejection of a held same-DO capability after replacement.

## Independent checks

From this directory, for each manifest (`Cargo.toml`, `fallback/Cargo.toml`):

```sh
cargo fmt --manifest-path fallback/Cargo.toml --check
cargo test --manifest-path fallback/Cargo.toml --all-targets --locked
cargo test --manifest-path fallback/Cargo.toml --doc --locked
cargo clippy --manifest-path fallback/Cargo.toml --all-targets --all-features \
  --locked -- -D warnings
cargo clippy --manifest-path fallback/Cargo.toml --target wasm32-unknown-unknown \
  --locked -- -D warnings
# Repeat with --manifest-path Cargo.toml for the minimal rejected-host repro.
npm run check:js
for f in gate-test.mjs mcp-test.mjs fallback/*.mjs; do node --check "$f"; done
cargo tree --manifest-path fallback/Cargo.toml --locked \
  --target wasm32-unknown-unknown -e features
```

Root checks run separately: `cargo fmt --check`, `cargo test --all-targets`,
`cargo test --doc`, `cargo clippy --all-targets --all-features -- -D warnings`.
Root checks do not cover the independent lab crates. See [fallback/README.md](fallback/README.md)
for the ABI, error policy, capture boundary and explicit scaling limits.
