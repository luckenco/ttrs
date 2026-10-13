# Stage-1 evidence ledger

Run: **2026-10-13**, Apple M5 Max, arm64 macOS / Darwin 25.6.0.
**COMPLETE — select TS Worker + TS SQLite Durable Objects + per-DO disposable,
synchronous Rust Wasm instances. Stop for review before stage 2.**

Reproduce with [README.md](README.md). [observations.json](observations.json) retains
selected-host measurements and both failed Rust-first lifetime schedules. The remote
product is not implemented; the proofs use synthetic data, not business-rule parity.

## Grounding and preserved contracts

Preserved parent `zltnyxms / 074e758e` contains the design; the original experiment
is `kmsxpsql / 4591dec0`. This follow-on change leaves production `src/`, `tests/`,
root Cargo manifests/lockfile and the existing product README untouched.

Actual dependency seam, not missing rules:

- `Tracker` owns concrete `Store` and forwards checked start/end/switch/dashboard
  operations (`src/tracker.rs`). `Store` owns rusqlite `Connection`.
- Start/end/checked end/switch/log open **IMMEDIATE** transactions, validate and
  execute SQL, commit on success, and roll back on error/drop (`src/store.rs`).
  `add_client` is a single statement; later revision bookkeeping must cover it too.
- Switch checks the complete captured timer and selected terms, ends old work and
  starts the new timer atomically. Failure after inserting the old entry must undo
  that entry and timer deletion.
- `src/store/timers.rs` takes concrete `Transaction`; billing/dashboard take
  concrete connection/rows. Typed numeric binds, row decoding, affected-row counts
  and `last_insert_rowid` couple existing rules to rusqlite. The shared seam needs
  exact casts and RETURNING, not a second rule engine or per-operation TS SQL port.
- Billing aggregates Rust i128 with current truncation order; calendar is Jiff-only
  with compatible-midnight/DST semantics. Dashboard already performs consistent
  read transactions. Trusted stored-row assertions must not decode untrusted DTOs.

Preserved: one timer/client, selected terms, full-value recovery checks, nonreused
IDs, atomic switch, rejection versus truncation, historical/future and mixed-month
work, immutable billing zone, DST and exact arithmetic. No production rule was
copied/extracted to compile the lab; business parity belongs to later stages.

All four reviewed product contracts remain unchanged and unimplemented here:
original-principal-bound recovery; reviewed-authority submission; fencing **all five**
legacy tables plus the write-rejecting `clients` sentinel on every new-schema path;
fresh suspend-aware server-time anchoring. Product protocol, complete portable
restore/fencing and no remote-to-local runtime fallback remain mandatory.

## Pins and tooling

| Component | Exact version |
| --- | --- |
| Rust / Cargo / target | 1.98.1; `wasm32-unknown-unknown`, no Emscripten |
| Selected core Jiff / tzdb | 0.2.35 / 0.1.9; defaults off, `std` + `tzdb-bundle-always` |
| serde_json | 1.0.154, both independent lockfiles |
| Rejected-host worker / worker-sys / worker-macros / worker-build | 0.8.6 |
| Rejected-host wasm-bindgen / js-sys / wasm-bindgen-futures | 0.2.128 / 0.3.105 / 0.4.78 |
| worker-build auxiliary tools | wasm-opt 132; esbuild 0.28.2 |
| Node / npm | 24.21.0 / 11.19.0 |
| cf | 1.0.0-beta.14 |
| Vite / Cloudflare Vite plugin | 8.1.5 / 2.0.0-beta.sha-91c870c02 |
| Cloudflare config / build-output-utils | 0.24.1 / 0.9.0 |
| Miniflare / workerd / compatibility date | 5.20261006.1-alpha / 1.20261006.1 / 2026-10-06 |
| Agents / MCP server / Zod / TypeScript | 0.28.1 / 2.3.1 / 4.6.5 / 5.9.3 |

Pins/lockfiles are spike-local. `npm ci` passes without forcing peer resolution.
Agents requires MCP server 2.3.1; the earlier documentation's 2.0.0 example failed
peer resolution. cf delegates to pinned Vite; Rust compilation is explicit. Wasm
is a module, not a public asset. Workspace/Authority SQLite exports and private
MCP READ_SERVICE are inspected and loaded in actual workerd. No Wrangler, SDK fork,
artifact patch, alternate database, credentials or remote resources were used.
Miniflare 5 uses `convertV4MiniflareOptions` and `resourcePersistencePath`.

Selected ABI: two synchronous imports (`sql`, `emit`), no SDK futures, ambient router
or retained pointers. Raw SQL cells must be strings/null. Unexpected SQL exceptions
and traps discard the instance centrally, including pre-route diagnostic failures.
Domain returns become callback exceptions after Rust returns normally, then a
receipt-only transaction rechecks without awaiting. Synthetic constraint failures
are infrastructure probes; typed recoverable SQL errors are a stage-2 design task.

## Complete selected-host runtime ledger

**Common command for every row:** `npm ci && npm run build && npm run check:js && npm test`
from this directory. The **same cf artifact** passes every row. Individual assertions
live in the named test modules; compilation/source inspection alone is not runtime proof.

| Required capability | Actual assertions/observations | Status |
| --- | --- | --- |
| Build/exports | Normal Wasm core resolves bundled New York timezone; no rusqlite, terminal/filesystem/system-zone dependencies. Actual cf JS/Wasm loads both SQLite Workspace/Authority exports. SDK MCP worker uses a private binding. | **PASS** |
| Exact SQL | `fallback/test.mjs`: generic borrowed query/text-null binds and row results; i64 MIN/MAX and ±9007199254740993; NULL; INSERT RETURNING; canonical overflow/leading-zero/bad spelling rejection in Rust; numeric raw result rejection; STRICT, FK, unique partial index; Jiff MAX timestamp/microseconds, exact i128/nanos text. Sequence high-water above 2^53 survives deleted maximum, eviction/restart and next allocation. Backup independently asserts all three numbers columns, including exact primary-key IDs. | **PASS** |
| Synchronous atomicity | Commit state/revision/success receipt together. Partial-write domain Err rolls back before separate rejection receipt; existing receipt wins. Actual failing SQL import and Wasm unreachable roll back with no infrastructure rejection receipt. Negative control returns ordinary Err data and commits. A mutating read is rejected/rolled back by actual cursor rowsWritten. | **PASS** |
| Rejection/lifetime | Both held-sibling schedules settle within three seconds after another DO traps; sibling keeps its own instance and writes only its SQL. Same-DO held JS capability is rejected after replacement without invoking its old exports or discarding the healthy new instance. Every escaped Rust frame discards, including an oversized pre-route read; repeated refusal preserves all 4,096 sequence rows. No Wasm frame, borrowed buffer or callback survives an await. | **PASS** |
| Isolation/durability | Workspace/Authority and named DOs are isolated. A real `unsafeEvictDurableObject` call uses workerd's graceful eviction path: constructor/instance changes, acknowledged SQL/exact values/receipts remain, sibling stays live. Separately restart workerd with the same disk root and verify sequence's next value. | **PASS** forced graceful eviction/restart; automatic inactivity timing not claimed |
| Auth/MCP SDK | `auth-test.mjs`, `mcp-test.mjs`: SHA-256 all configured credentials and native timingSafeEqual on fixed-width digests; ≤512-byte Authorization; stable principal across read rotation; forged roles ignored. Worker and private DO independently authenticate original credentials. Denied reads/mutations leave persisted assertions unchanged. SDK initialize/list/call, read-only tool, rejected grants/unknown mutations, Host/Origin allowlists. Both ingress paths count actual 4,096-byte streamed bodies, including chunked multibyte UTF-8. Malformed bytes/JSON fail. | **PASS** lab role model, not production auth/protocol. No timing benchmark is claimed; the cryptographic primitive is exercised. Lying Content-Length may be normalized/rejected by transport, not counted as server proof. |
| R2/alarms SDK | `backup-test.mjs`: one synchronous capture includes all synthetic columns/receipts/sequence and logical operational metadata. Persist immutable bytes before awaits; actual emulated R2 payload exists while injected before-manifest failure leaves no manifest. Restart then real alarm retries original bytes, one completion, matching sizes/counts/SHA-256. Before-payload failure leaves neither object. Actual alarm-handler automatic retry has isRetry; explicit rearm survives restart. Completed reruns do not rewrite either object. Held R2 I/O plus a concurrent command cannot alter captured bytes. | **PASS** platform capture/publication/retry gate; no product restore/import verifier claimed |
| Costs | `cost-test.mjs`: indexed crossings, 1/120/2400-month tiny-data pattern; complete capture at 256/257/1000/10000 rows; unpaused latency; Rust allocator live/peak; Wasm capacity; named workerd inspector JS/external heap sampled synchronously inside emit while full Rust rows/serialized JSON/parsed JS coexist. Concurrent core-only 1/8/32 identities and 32 completed eviction requests measured. cf startup profiles are separate. | **PASS** local measurements with explicit accounting/production limits below |

## Costs and limits, not production capacity promises

The checked-in JSON is authoritative for this run; profiler windows are not
production cold starts. Selected bundle is about **728 kB**; optional MCP about
**761 kB**, separately deployed worker. The selected startup profile has one sample
and cannot resolve useful active-load time; MCP samples show actual SDK load work.
Per-DO Wasm construction is also timed with millisecond-resolution Date.now:
0–1 ms in the controlled 32-identity sample.

| Synthetic capture cost rows | All rows | JSON bytes | Crossings | Unpaused ms | Rust peak requested bytes | Wasm capacity bytes |
| --- | --- | --- | --- | --- | --- | --- |
| 256 | 260 | 3,180 | 10 | 1 | 85,530 | 1,507,328 |
| 257 | 261 | 3,192 | 10 | 0 | 85,632 | 1,507,328 |
| 1,000 | 1,004 | 12,233 | 12 | 1 | 133,561 | 1,572,864 |
| 10,000 | 10,004 | 130,734 | 48 | 8 | 858,835 | 2,752,512 |

These figures describe the recorded run, not stable latency bounds. Eight tiny
sections plus 256-row cost pages are captured in **one synchronous turn**, not
paged over awaits. SQL output scratch is 64 KiB. The eight other tiny sections are
unpaged; exceeding that per-query result cap refuses without deleting/truncating
history. Real collection paging/row limits must be designed during core extraction.

At the 10,000-row emit point, the recorded inspector fields are about 3.60 MB used
JS heap / 6.82 MB total JS heap, 0.77 MB embedder heap and 0.36 MB backing storage.
These are isolate-wide samples, including other live DOs/garbage, not a sum, pure
per-capture delta or certified maximum RSS. Rust requested allocation peaks exclude
allocator overhead; Wasm capacity includes the Rust heap and cannot be added to
external/backing accounting. Native SQLite page-cache/process memory is not measured.
No forced GC/reclamation is claimed: the exploratory collectGarbage command timed
out at eight seconds in this pin; the operative suite measures raw heap usage and
pauses/resumes successfully. Capture latency is measured **without** the pause.

128 primary-key SQL crossings measured 0 ms at Date.now resolution (earlier run: 1 ms). On twelve stored rows, 1/120/2400
calendar months produce 1/120/2400 indexed crossings (about 2/1/10 ms) plus exact
Rust/Jiff i128 aggregation. Index use is static schema/query review, not runtime
EXPLAIN. This remains **O(months)** even with tiny history: wider spans can exhaust
CPU/crossing budgets. Future admission/query redesign must account for that.

Each core-only live DO has **1,507,328 bytes** of Wasm capacity; 8/32 have
**12,058,624 / 48,234,496 bytes** before real rules, Jiff caches and capture growth.
Inspector concurrency samples share a contaminated baseline from preceding tests;
completed eviction requests do not establish forced GC or reclaimed-memory bounds.
A provisional **eight live identities / 64 MiB planning budget**, **512 KiB logical
backup**, **one pending/eight completed jobs**, **4,096-byte ingress** are lab limits,
not production admission grants. Tested histories stop at 10,000 synthetic cost rows.
Unbounded live identities or growing unpaged receipts can exhaust memory/refuse
capture; commands/history must be preserved, with explicit capacity refusal and a
later measured paging/staging/admission redesign—not silent truncation.

Logical captures include pending/completed job metadata and completion receipts;
internal `lab_jobs.payload` staging is excluded like a WAL to avoid recursive
self-embedding. No secrets or grant/routing state are captured. This is a platform
proof around a complete **synthetic** logical schema, not product restore, imported
receipt replay, retention deletion or a native↔DO restore drill.

## Host choice and rejected-runtime repro

Rust-first was attempted. Pinned worker-build glue shares/replaces a Wasm runtime.
After an actual trap, both the trapping request and a sibling Rust SDK future already
suspended on I/O remain unsettled at the **two-second bound** in both schedules.
Fresh requests work, SQL rolls back and no stale mutation is observed; those facts
do not prove safe in-flight recovery.

`npm run build:rust-repro && npm run test:rust-repro` still exits **1** after reducing
the losing harness to three small Rust modules. Its narrow zero-argument ScopedClosure
transaction binding and real SDK fetch await preserve the blocker without SDK/glue
surgery. The full losing capability harness was removed after selection; historical
measurements are explicitly labeled with parent revision 4591dec0.

**Select the thin TS platform host.** It keeps shared-rule ownership in Rust, binds
SQL to each disposable instance, and keeps async effects outside Rust stack frames.
Stage 2 remains gated on user review: extract the real shared SQL/core boundary,
then prove native/Wasm business parity. Product journal/protocol/authority/migration,
backup/restore safety contracts and production capacity remain later-stage work.

## Verification and source-control handoff

Passed: `npm ci`, selected cf build, TypeScript and all JS syntax checks, the complete
actual-workerd suite and both startup profiles. Both independent crates pass fmt,
locked native tests/doctests and native/Wasm Clippy with warnings denied. The selected
core has **3 unit tests + 1 doctest**; the minimal SDK repro has no native tests and
is verified by its runtime blocker. Root fmt, **114 tests + 8 doctests** and
all-target/all-feature Clippy pass. Generated artifacts/state/profiles stay ignored.

Expected failure: rejected Rust-host lifetime repro, both schedules. This is separate
from green selected-host `npm test`. No provisioning, deployment, login, bookmark
movement or push. Follow-on JJ change: `sqwrlqns`, above `kmsxpsql / 4591dec0`.
