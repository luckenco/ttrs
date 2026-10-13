# Remote-host feasibility lab — stage 1 only

**Stage 1 is incomplete. No production host is selected.** See [evidence.md](evidence.md)
and the checked-in [observations.json](observations.json). Rust-first passes many
capabilities but leaves trapping and already-suspended sibling requests unsettled.
The thin TS fallback passes the focused lifetime/transaction experiment, not the
entire ledger. Stop for review; do not start the shared-storage refactor.

This directory is independent of the root Cargo manifest/lockfile. Neither crate
imports `tt`, copies billing rules, or implements a remote workspace. All SQL and
data are synthetic. `Workspace`/`Authority` are actual exported SQLite DO classes,
not implementations of product routing/restore. The optional TS adapter contains
one established SDK read tool and no SQL/domain rules.

## Safety

- **Never deploy this lab.** Its credentials are deliberately public test fixtures.
- No account ID, management token, real tracking data, remote binding, provisioning,
  authentication/login, or deployment is needed or permitted.
- Vite explicitly disables remote bindings and tunnels, binds only loopback port
  8799, and uses project-local `.cloudflare/state`. Tests use actual workerd through
  pinned Miniflare with separate temporary persistence, not production services.
- The held-gate service controls external I/O timing only; SQL/transactions/Wasm
  are not mocked. No Wasm generated-artifact patches or SDK forks are used.
- Failed remote work does not fall back to local tracking. `fallback` here means
  an isolated **host experiment**, never a runtime/product fallback.

## Clean-checkout reproduction

Prerequisites: Node **24.21.0**, npm **11.19.0**, rustup, network access to obtain
pinned packages/build tools. Tested on Apple M5 Max / arm64 macOS (Darwin 25.6.0).
The toolchain file pins Rust **1.98.1**, rustfmt/clippy and the normal Wasm target.

```sh
cd spikes/remote-host
rustup toolchain install 1.98.1 --profile minimal --component rustfmt,clippy \
  --target wasm32-unknown-unknown
# Isolated executable installation, not a moving global worker-build:
cargo +1.98.1 install worker-build --version 0.8.6 --locked --root .tools
export PATH="$PWD/.tools/bin:$PATH"
npm ci
mkdir -p results

npm run build                        # worker-build -> cf -> Vite; Rust-first
npm run check:js
npm run test:capabilities             # passes; about 60 seconds for real alarm
npm run test:mcp                      # cf-built SDK adapter -> private Rust binding
npm run test:gate                     # EXPECT exit 1: recorded Rust lifetime blocker
# npm test runs all three; its overall exit 1 is NOT stage-1 success.
npm run check:startup > results/rust-startup.json

npm run build:fallback               # isolated Rust capability core + thin TS host
npm run test:fallback                # passes only the documented focused subset
./node_modules/.bin/cf workers check --prebuilt --mode fallback \
  --outfile results/fallback-startup.cpuprofile > results/fallback-startup.json
./node_modules/.bin/cf workers check --prebuilt --mode fallback --worker read-mcp \
  --outfile results/mcp-startup.cpuprofile > results/mcp-startup.json
```

`cf build` does not compile Rust: use the scripts. Mode `local` is rejected by
Vite's `.env.local` convention; the local-only experiment uses mode `spike`.
Switching mode overwrites ignored `.cloudflare/output`; rebuild the appropriate
mode before running its tests. All tests load the actual cf output JS/Wasm and
reject mismatched shapes. Do not substitute Wrangler.

Named individual Rust-first probes, after `npm run build` and `npm run dev`:

```sh
curl -H 'Authorization: Bearer lab-write-only' \
  http://127.0.0.1:8799/workspace/synthetic/i64-extremes
# Replace i64-extremes with rollback-err, rollback-js, negative-control,
# lifetime, isolation, costs, commit, state, backup, schedule, alarm-state.
curl --max-time 2 -H 'Authorization: Bearer lab-write-only' \
  http://127.0.0.1:8799/workspace/synthetic/trap  # expected timeout; inspect state
```

`gate-test.mjs` supplies `LAB_GATE` and proves two Rust DOs share a Wasm runtime,
then holds B across A's trap in replacement-first and callback-first orderings.
`fallback/test.mjs` repeats those orderings with separate DO-owned instances and
requires settled responses within three seconds. Neither timeout counts as a pass.
`test.mjs` independently checks persistence by disposing/restarting workerd with
the same storage root; the `restart` endpoint alone is not evidence of a restart.

Fresh schemas are lab bootstrap only. Existing Rust read probes and MCP do not
write SQL; authentication/role checks precede object resolution. The fallback's
fixture auth is intentionally incomplete, as its README and ledger state.

## Checks (both independent crates)

```sh
cargo fmt --check
cargo test --all-targets --locked
cargo test --doc --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo clippy --target wasm32-unknown-unknown --locked -- -D warnings
cargo fmt --manifest-path fallback/Cargo.toml --check
cargo test --manifest-path fallback/Cargo.toml --all-targets --locked
cargo test --manifest-path fallback/Cargo.toml --doc --locked
cargo clippy --manifest-path fallback/Cargo.toml --all-targets --all-features \
  --locked -- -D warnings
cargo clippy --manifest-path fallback/Cargo.toml --target wasm32-unknown-unknown \
  --locked -- -D warnings
npm run check:js
node --check test.mjs
node --check gate-test.mjs
node --check mcp-test.mjs
node --check fallback/test.mjs
cargo tree --locked --target wasm32-unknown-unknown -e features
```

Root checks, run separately from the repository root:
`cargo fmt --check`, `cargo test --all-targets`, `cargo test --doc`, and
`cargo clippy --all-targets --all-features -- -D warnings`.

## Evidence artifacts

`observations.json` preserves the measured run, including failures, without
committing generated bundles, databases, node_modules, target trees or CPU profiles.
Repeated runs regenerate ignored `results/workerd.json`, `results/gate.json` and
startup profiles. Fallback test results print JSON; redirect them to
`results/fallback.json`. The capability test retains its temporary synthetic
storage path in its report for inspection; the fallback deletes its temporary
storage. Vite state and startup profiles are also ignored.

See [fallback/README.md](fallback/README.md) for the small synchronous raw-Wasm ABI
and explicit missing gates. Preserve both minimal experiments while host choice
is blocked; after a complete selection, remove the losing harness as the handoff
requires. No product scaffold under `crates/tt-worker` or `worker/` was created.
