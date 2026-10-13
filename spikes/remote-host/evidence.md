# Stage-1 evidence ledger

Run: **2026-10-13**, Apple M5 Max, arm64 macOS / Darwin 25.6.0.
**BLOCKED / INCOMPLETE — no selected production host; stop before stage 2.**
Reproduce with [README.md](README.md). Machine-readable measurements and the
failed lifetime schedules are retained in [observations.json](observations.json).
Compilation, source inspection and ordinary capability-test success are not
substitutes for the open gates below.

## Grounding and preserved contracts

The original JJ change `zltnyxms / 074e758e` contained README/design documents.
This lab is a child change; root `src/`, `tests/`, `Cargo.toml`, `Cargo.lock` and
that README work remain unchanged. Canonical design/sketch/rationale were read.
Only status/evidence links were added to the design and rationale.

Concrete seam, not missing rules:

- `Tracker` owns a concrete `Store` (`src/tracker.rs:7–24`) and forwards checked
  start/end/switch/dashboard operations (`186–260`).
- `Store` owns rusqlite `Connection` (`src/store.rs:90–115`). Start/end/checked
  end/switch/log open **IMMEDIATE** transactions (`365–477`), validate and execute
  SQL, and commit only after success. Dropping the transaction on `?` rolls back.
  `add_client` is currently a single statement (`123–131`), not an explicit
  IMMEDIATE transaction; later revision bookkeeping must cover it too.
- Switch checks the entire captured timer and same client, then ends old work and
  starts selected new terms in one transaction (`store.rs:405–433`). Failure after
  the old entry insertion must roll back that insertion and timer deletion.
- `src/store/timers.rs:8–103` takes concrete `Transaction`, finds the selected
  agreement/project, checks current terms/client timer, calls billing, inserts an
  entry and deletes exactly one timer. It uses typed numeric binds,
  `last_insert_rowid` and affected-row counts; the future seam needs casts and
  RETURNING, not a second rule engine.
- `billing.rs:54–178` and `185–270` take concrete `Connection`, retain selected
  agreement, cap/proration/coverage behavior, and aggregate intervals with Rust
  i128. `calendar.rs:9–41` is Jiff-only; compatible-midnight/DST semantics must stay.
- `dashboard.rs` opens consistent read transactions, reads typed rusqlite rows and
  performs integer earnings aggregation. `store.rs` row constructors assume trusted
  stored values; those assertions must not decode untrusted DTOs directly.

Preserved: one timer/client, selected terms, full-value recovery checks, nonreused
IDs, atomic switch, log rejection versus stop truncation, historical/future and
mixed-month work, immutable project billing zone, DST and integer truncation order.
No rule was copied or extracted to get this lab to compile. Synthetic SQL/capability
expressions do not prove native/Wasm **business** parity; that belongs later.

All four reviewed product contracts remain unchanged and unimplemented here:
original-principal-bound recovery; reviewed-authority submission; fencing **all five**
legacy tables plus the write-rejecting `clients` sentinel on every new-schema path;
and fresh suspend-aware server-time anchoring. Their later acceptance tests,
complete backups/restore fencing, and no remote-to-local runtime fallback remain
mandatory. Lab schema bootstrap is not a proposed migration.

## Pins and tooling decisions

| Component | Exact run version / pin |
| --- | --- |
| Rust / Cargo | 1.98.1; rustc `48a229cea`, Cargo `797e8a9bc`; `rust-toolchain.toml` |
| Target | `wasm32-unknown-unknown`, stable abort strategy; no Emscripten |
| worker / worker-sys / worker-macros | 0.8.6; first two explicit pins, macros locked |
| worker-build executable | 0.8.6; checked installed manifest; reproducible isolated install in README |
| wasm-bindgen library + downloaded CLI | 0.2.128 |
| js-sys / wasm-bindgen-futures | 0.3.105 / 0.4.78 |
| Jiff / Jiff tzdb | 0.2.35 / 0.1.9, `std` + `tzdb-bundle-always`, defaults off |
| subtle | 2.6.1, Rust-first constant-time credential comparison |
| worker-build auxiliary tools | wasm-opt 132; esbuild 0.28.2 (pinned tool source) |
| Node / npm | 24.21.0 / 11.19.0 |
| cf | 1.0.0-beta.14, project-local |
| Vite / Cloudflare Vite plugin | 8.1.5 / 2.0.0-beta.sha-91c870c02 |
| Cloudflare config / build-output-utils | 0.24.1 / 0.9.0 |
| Miniflare / workerd | 5.20261006.1-alpha / 1.20261006.1 (`workerd 2026-10-06`) |
| Compatibility date | 2026-10-06, intentionally matches the pinned local runtime |
| Agents / MCP server / Zod | 0.28.1 / 2.3.1 / 4.6.5 |
| TypeScript | 5.9.3 |

Both isolated Cargo lockfiles and package-lock.json are committed. No root package
or lockfile changes. README records clean-checkout commands; `npm ci` was run.

Source recheck: worker 0.8.6 has SQL but neither it nor worker-sys exposes
`transactionSync`; the async transaction API is not used. `worker/src/sql.rs` casts
i64 through f64. The lab uses only text/null binds/results and canonical Rust range
validation. One private binding in `src/transaction.rs` covers only transactionSync.
Zero-argument borrowed `ScopedClosure` returns `Result<JsValue, JsValue>`; the ABI
throws **after locals drop**, avoiding `throw_val`'s destructor-skipping path.

worker-build 0.8.6 enables wasm-bindgen reset/abort glue and proxies classes over a
shared runtime. Inspection of generated code and `wasm-bindgen/src/handler.rs`
confirms reset constructs a **new WebAssembly.Instance**, rather than repairing
trapped memory. Runtime IDs prove sharing and replacement in actual workerd.

Packaging decision: keep the requested **cf** workflow, delegated to pinned Vite
plugin 2 beta. `worker-build` runs explicitly before cf; cf does not run the package
build script. Vite emits Wasm as a module, not a public asset. Actual manifest has
Workspace/Authority SQLite exports, namespace bindings and a Wasm module. The MCP
auxiliary worker is also cf-built with `READ_SERVICE` private service binding.
No Wrangler CLI, SDK fork, generated-artifact patch or remote resource was used.
Mode `local` failed (reserved by Vite); mode `spike` is local-only through explicit
`remoteBindings: false`, `tunnel: false` and loopback configuration. Miniflare 5
requires its versioned converter and **resourcePersistencePath**; obsolete v4
per-resource persistence options were dropped, initially causing a test failure.
Corrected tests prove persistence instead of accepting the in-memory result.
Agents 0.28.1 requires MCP server 2.3.1; the documentation's 2.0.0 example failed
peer resolution and was not forced.

## Runtime proof ledger

Versions above apply to every row. Commands run from this directory after the
appropriate build. `npm test` runs the Rust capability suite, MCP suite and lifetime
repro; **overall exit 1** is expected for the recorded blocker, not a green gate.

| Required capability | Exact commands / assertions and observations | Status / blocker |
| --- | --- | --- |
| Build/exports | `npm run build`; `npm run check:js`; `cargo tree --locked --target wasm32-unknown-unknown -e features`; real cf output loaded by `node test.mjs`. Jiff New York timezone resolves from the bundle. No rusqlite, terminal dependencies or Jiff system-zone features. Both SQLite DO classes execute. `npm run build:fallback && node fallback/test.mjs` likewise loads actual cf JS/Wasm. | **PASS** for both isolated artifacts. No shared business core linked yet, intentionally. |
| Exact SQL | Rust `i64-extremes` via `node test.mjs`: i64 MIN/MAX and ±9007199254740993, NULL, CAST input/output, INSERT RETURNING, numeric result rejection, STRICT type rejection, FK enforcement, unique partial index, sequence set above 2^53 then next ID 9007199254740994. Timestamp MAX `9999-12-30T22:00:00.999999999Z`, microseconds `253402207200999999`, nanoseconds as i128 text; exact wide product `85070591730234615847396907784232501249` crosses HTTP as string. Canonical overflow/leading-zero rejection also unit-tested. Fallback `exact` repeats wide values and sequence persistence. | **PASS** Rust synthetic suite. **OPEN** fallback numeric-result rejection, canonical validation, RETURNING/FK/partial-index gates; not inherited from Rust host tests. No complete snapshot import/restore proof. |
| Synchronous atomicity | `node test.mjs`: commit state+revision+receipt; `rollback-err` writes all three then returns Err; callback exception rolls back all three and Rust Drop guard runs; `rollback-js` is an actual workerd SQL/FK JS exception, no infra receipt. `negative-control` returns serialized Err as ordinary data and **commits**. `trap` executes Wasm unreachable after writes; later reads show no revision/state/receipt change. `node fallback/test.mjs` repeats domain/JS/trap/negative-control and requires HTTP 500 for trap. | **PASS** measured SQL atomicity on both. Rust trapping request itself times out; see lifetime row. JS exception is platform SQL exception, not a fabricated mock. |
| Rejection/lifetime | Rust repeated `rollback-err` / `lifetime`: separate receipt-only tx immediately after rollback, no await; existing receipt wins; infrastructure has no rejection receipt; captured borrowed callback invoked after Drop throws before Rust captures run. `node gate-test.mjs`: A/B have equal runtime IDs, hold B in real Rust SDK service-fetch await, trap A, release B before/after a fresh call. New runtime ID, clean SQL both identities, fresh calls usable, but **A and B requests remain unsettled at the two-second bound** in both schedules. Fallback repeats with per-DO instances; trap settles and B resumes using its unchanged instance. | **BLOCKED Rust safe in-flight recovery**. Replacement mechanism and absence of stale SQL mutations observed, not comprehensive lifetime safety. **PASS focused fallback lifetime** with no Wasm futures; no custom shared-glue fix attempted. |
| Isolation/durability | Rust two workspace identities plus Authority identity, repeated and Promise.all calls, distinct SQL markers; shared runtime ID established by gate repro. Dispose/restart workerd with same persistence; state, revision and receipts survive durable acknowledgments. Trap rollback/replacement followed by restart preserves committed state. Fallback held sibling writes only its SQL and retains distinct instance, both namespace identities remain separate; restart preserves values/receipts and restored sequence's next ID 9007199254740995. | **PASS** measured identities/restart/durable ack; **OPEN** actual inactivity-driven eviction (restart is stronger memory loss but not an eviction lifecycle test). Sibling Rust in-flight availability blocked as above. No arbitrary SQL API exposed. |
| Auth/MCP SDK | `node test.mjs` bounds Rust POST ingress to 1024 **streamed bytes** (including absent Content-Length), checks auth before routing and rejects read writes/forged headers. Two read tokens give stable lab-reader. `node mcp-test.mjs` loads cf auxiliary SDK server and private Rust read service: initialize/list/call, one read tool, rotation, uniform invalid/write credential rejection, forged grant/identity fields rejected, Host/Origin allowlists including null/malformed/evil origins, actual 4096-byte/chunked UTF-8 body limits. Original credential forwarded; Rust validates again, never trusts JSON grants. Existing reads/MCP do not mutate SQL. | **PASS Rust+optional SDK adapter** (lab role model only, no admin/production secrets). **OPEN fallback bounded/constant-time auth and MCP session/credential flow**. Fallback fixture string comparisons are not accepted for selection. HTTP client rejected an inconsistent Content-Length before workerd; this is not claimed as a server probe. |
| R2/alarms SDK | `node test.mjs`: synchronously capture synthetic data/receipts/high-water, persist immutable job payload, upload payload, inject failure before manifest. Actual emulated R2 contains payload and **no manifest**. Rearmed persisted alarm survives workerd restart, fires after ~60 s, retries to done/attempts 2, publishes manifest last and one completion receipt. Repeated job ID does not retry completed work; payload size matches manifest. | **PASS measured Rust manifest-last / persisted rearm / retry / idempotency subset**. Corrected SDK bug: workers-rs i64 set_alarm argument is an **offset**, not absolute Date.now; scheduled-bound assertion now catches this. **OPEN fallback R2/alarm gates**, alarm-handler failure/rearm matrix, full logical operational-metadata backup, digest verification and restore. |
| Costs | `node test.mjs` five samples `costs`, plus `cf workers check --prebuilt --mode spike|fallback` and `--worker read-mcp` (actual commands in README). Eight-table synchronous Rust synthetic capture, selective indexed crossings and 1/120/2400-month patterns on 256 payload rows measured below. Fallback baseline/after-exact memory and request timing retained. | **PARTIAL**: real local costs, not capacity certification. **OPEN fallback indexed/capture/calendar-query costs and full peak live JS+Rust heap measurement**, representative history/receipts/growth/concurrency. |

## Measured costs, not capacity promises

Local CPU profiles are tiny samples and **not cold-start predictions**. The Rust
and fallback default bundles exclude the separately deployed optional MCP worker.

| Artifact | cf bundle bytes / gzip bytes | Profile window / samples |
| --- | --- | --- |
| Rust Worker/DOs | 1,114,432 / 364,721 | 10.460 ms / 1 |
| TS host + Rust core | 614,300 / 179,066 | 13.586 ms / 1 |
| Optional MCP SDK adapter | 761,185 / 177,280 | 52.888 ms / 15 (31.496 ms active, 4.586 ms GC) |

Rust five cost requests: **6.32–10.10 ms** end-to-end local wall time. Worker
Date.now has millisecond resolution; zero means below resolution, not zero cost.
128 indexed ID SELECTs cross Rust/JS SQL 128 times, measured **0–1 ms** internally.
Capture includes synthetic data, state, retained outcomes and sequence in one
synchronous turn: **283 rows**, 8 queries, **10,423 JSON bytes**, serialized Vec
capacity **16,384 bytes**, **0–1 ms**. Database: **77,824 bytes**.
Linear memory before first capture: **24 pages (1,572,864 bytes)**; after capture:
**25 pages (1,638,400 bytes)**, stable in later samples. This is linear-memory
high-water, **not** live allocator peak, total JS heap or process RSS.

Tiny-data calendar query pattern: one indexed query and two Jiff zone conversions
per month, **1/120/2400 SQL crossings**. 1 and 120 months measured below 1 ms in
these samples; 2400 months **4–6 ms**. This is a synthetic query/zone pattern,
not copied billing. Actual capped paths can need additional agreement/usage
statements per month (`billing.rs:54–270`); old timers and long spans can grow
O(months) even with tiny storage. Uncapped `ensure_capacity` already short-circuits;
that behavior must not be lost. No whole-history-per-command plan is proposed.

Fallback instance baseline: **1,376,256 bytes (21 pages) per live DO**, after the
exact/Jiff span probe **1,441,792 bytes (22 pages)**. Trap responses ~0.99–1.14 ms
local wall time; held sibling requests ~2.74–5.80 ms including gate coordination.
A separate Wasm memory per live DO scales with **live identities**, not merely
stored rows; bundled tzdb contributes to each instance. This cost needs a measured
concurrency budget before choosing the fallback. The two bundle profiles do not
have equal feature coverage and cannot rank production hosts yet.

Proposed **next-lab** limits (not production guarantees): keep the demonstrated
256 payload-row / 16 KiB export workload and one pending alarm job; test bounded
1 MiB / 10k-row capture tiers before considering them. Use 120-month spans as the
first regular query budget experiment and 2400-month spans as a stress case.
Measure full heap, growth, retention and concurrent live DOs; set production
admission/import/snapshot limits only then. Larger local data remains preserved,
not truncated; fail remote preflight rather than make complete backup impossible.
Do not tighten full-range integer/date semantics to hide a host limitation.

## Host decision and remaining work

**No host selected.** Rust-first is blocked on safe handling of already-suspended
futures during shared-runtime replacement. There was no observed cross-SQL write or
memory-corruption proof; the concrete failure is unsettled requests after trap
and gate release. The two-second test bound is a lab recovery policy, not a claimed
Cloudflare SLA. The suspected generated closure/reset ordering deserves upstream
review; avoid assuming that later healthy calls prove every old future safe.

The focused fallback solves that schedule by keeping async platform work in JS
and one disposable synchronous Rust instance per DO. It is **not yet a passing
full fallback host**: static statement IDs/raw ABI are a minimal lifetime repro,
not the future generic shared SQL seam. Authentication, exact-adapter constraints,
MCP, complete backup/operational metadata, alarms and capture/crossing costs must
run against the fallback shape too. Keep both minimal harnesses pending selection;
remove the losing implementation only after all rows pass on one shape.

Stop for review now. Continuing stage 1 means complete the fallback ledger or
resolve/prove the pinned Rust lifetime issue without broad SDK/glue surgery.
Stage 2, production storage extraction, imports/restores and deployment are not
started. Full business parity and the four reviewed safety contracts stay later
acceptance gates; nothing here relaxes them.

## Verification / source-control handoff

Passed root: fmt; all-target tests **114**; doctests **8**; all-target/all-feature
Clippy with warnings denied. Passed isolated crates: fmt, native tests **2 + 1**,
zero doctests, native and Wasm Clippy with warnings denied, locked Wasm builds.
Passed npm ci, TypeScript, JS syntax checks, cf packaging, Rust ordinary capability
suite, cf-built MCP suite and focused fallback workerd suite.
**Failed:** Rust lifetime repro (both schedules); `npm test` exits 1 accordingly.
Open/partial behavior is listed explicitly in the ledger, not marked passed.

JJ child change `kmsxpsql` follows preserved `zltnyxms / 074e758e`; no bookmark was
moved, no push or remote deployment/provisioning/login occurred. Generated build,
state, target and result files are ignored; only source, locks and curated evidence
are retained. Commit hashes for the mutable child will change as JJ snapshots it.

Sources rechecked: pinned registry worker/worker-sys/sql/durable, worker-build
shim/build versions, wasm-bindgen closure/reset and js-sys futures; installed cf,
Vite/config and Miniflare 5 schemas. External references:
[DO SQL/transactionSync](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/),
[Rust Workers](https://developers.cloudflare.com/workers/languages/rust/),
[cf projects](https://developers.cloudflare.com/cf/projects/),
[typed config](https://developers.cloudflare.com/cf/projects/cloudflare-config/),
[MCP handler API](https://developers.cloudflare.com/agents/model-context-protocol/apis/handler-api/).
