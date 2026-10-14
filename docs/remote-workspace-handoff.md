# Remote-host feasibility spike: completed dispatch

**Stages 1–3 are complete.** [Host evidence](../spikes/remote-host/evidence.md)
selects the thin TS host with synchronous, disposable Rust instances. Stage 2 was
approved and implemented after review; [native-seam evidence](remote-workspace.md#stage-2-native-seam-evidence)
records its contracts and checks. [Stage-3 evidence](remote-workspace.md#stage-3-data-safety-evidence)
and [native data-safety contracts](data-safety.md) cover migrations, portable snapshots
and native restore. Stage 4 operation service/protocol is next; maintenance CLI and
remote mode remain unimplemented. Retained receipts will require a new portable format.
The original stage-1 dispatch below is retained as its historical acceptance contract.

## Original dispatch scope

Implement **stage 1 only**, then report and stop before stage 2. Remote mode is not
implemented; the current working change contains design documents, not a remote
backend. The preferred experiment is **workers-rs Rust Worker + Rust Durable Objects
on `wasm32-unknown-unknown`**, with a thin TS platform host as the fallback.

Product-facing usage is unchanged: users select local/remote, and Rust callers use
`Workspace::read`, `apply(expected_authority, operation)`, `request_status` and `retry`.
These APIs are still sketches. This task proves the host, not the entire product.

## Read and ground

1. Read repository `AGENTS.md`, then these canonical documents completely:
   - [Design](remote-workspace.md): behavior, safety contracts and implementation stages.
   - [Type sketch](remote-workspace-sketch.md): application/protocol/host ownership.
   - [Rationale](remote-workspace-rationale.md): original synthesis and Rust-host revision.
2. Inspect current JJ state and source; preserve existing work. Trace
   `Tracker -> Store IMMEDIATE transaction -> timers/billing -> SQL -> commit` through
   `src/tracker.rs`, `src/store.rs`, `src/store/{timers,billing,calendar,dashboard}.rs`.
   The seam is concrete rusqlite statements/rows/transactions, not missing rules.
3. Recheck pinned upstream APIs and installed tooling. The planning-time observations
   in the design are unpinned and do not establish runtime feasibility.

Done when the evidence report identifies the actual dependency seam, preserved
invariants and pinned build/runtime versions—not merely a list of files read.

## Build the smallest isolated lab

Use `spikes/remote-host/` as a self-contained lab, outside product `src/` and the root
manifest/lockfile. Record clean-checkout build/test commands in its README. Start
with a tiny Rust/Jiff capability harness; do not copy or port billing rules or
extract production storage to make the experiment compile. Full business parity
belongs to the later shared-core stages.

Pin `worker`, `worker-build`, `wasm-bindgen`, Rust toolchain/target, workerd and cf
versions, including any worker-sys/raw binding used. Inspect generated JS/Wasm,
class names and bindings. Prefer the cf workflow already specified; prove it can
package the required artifacts. A tooling change is a reported decision, not an
unannounced Wrangler substitution.

The proposed paths `crates/tt-worker/` and `worker/cloudflare.config.ts` describe
later product ownership; creating those scaffolds is not stage-1 completion.
Use local workerd/emulated bindings only. Remote deployment, resource provisioning,
production credentials and real tracking data are outside this dispatch.

## Prove and record

Maintain `spikes/remote-host/evidence.md`: for each row record versions, exact command,
assertions/observations, pass/fail and remaining blocker. Documentation, compilation
and mocks alone are not proof. Named probes such as `i64-extremes`, `rollback-err`,
`trap`, `isolation` and `restart` make failures reproducible.

| Capability | Required evidence |
| --- | --- |
| Build/exports | Minimal Rust/Jiff on normal Wasm without native SQLite/terminal/filesystem/system-zone dependencies; actual Workspace/Authority SQLite DO exports and cf artifact loading in workerd. |
| Exact SQL | Text/null casts round-trip full i64 extrema and values beyond 2^53, `Timestamp::MAX`, exact i128 result encoding; numeric-result rejection, RETURNING, STRICT, FKs, partial indexes and sequence high-water restoration. |
| Synchronous atomicity | State/revision/success receipt commit together. Partial-write domain Err, thrown JS exception and Wasm trap roll back. Negative control: Err returned as ordinary callback data commits, demonstrating why exception translation is required. |
| Rejection/lifetime | Record domain rejection only after rollback in a separate transaction, without an intervening await; existing receipt wins recheck. Infrastructure failures/traps produce no rejection receipt. Prove callback lifetimes and safe replacement of trapped runtime state. |
| Isolation/durability | Interleave two DO identities, including any shared generated Wasm runtime; neither can access the other's SQL. Also test separate instances, repeated calls, restart/eviction persistence and durable acknowledgement. |
| SDK capabilities | Small local probes for bounded authenticated ingress, stable principal/read roles, established SDK MCP with Host/Origin checks, R2 payload/manifest-last outside SQL, and persisted alarm rearming/idempotent retries. Prove any optional MCP adapter's private binding/credential flow. |
| Costs | Measure bundle/startup, indexed SQL crossings, full synchronous capture memory/latency and long calendar-span query patterns on tiny data. Report measurements and proposed limits, not invented capacity promises. |

No public `Storage::transaction_sync` API is assumed. Inspect the pinned crate and
worker-sys/raw access. One small private binding is acceptable if actual workerd
proves zero-argument synchronous callbacks, Err-to-exception rollback, borrow/callback
lifetimes and trap recovery. Async `Storage::transaction()` does not by itself satisfy
this contract. Generated glue may share a Wasm runtime: separate Rust DO structs
are not automatically proof of isolation or safe recovery.

## Select a host or stop

- **Rust passes:** select Rust Worker + Rust DOs linking the shared core directly.
  Optional TS adapts an established read-only MCP SDK only; it owns no SQL,
  transactions or domain rules. Keep its private service access authenticated.
- **Rust needs substantial workarounds:** record minimal repros, then try the thin
  TS Worker/DO host around the same Rust core against the same gates. SDK forks,
  broad custom binding surfaces, repeated artifact patching or ambient mutable
  storage routing are reasons to change host, not expand the workaround.
- **Neither passes:** report blocked and redesign. Full-range integers, rollback,
  identity fencing and complete backups are requirements, not negotiable workarounds.

Emscripten's experimental in-memory filesystem is not DO persistence. Keep managed
SQLite; DuckDB, an in-memory SQLite replica, a TS billing port and a handwritten MCP
protocol are not rescue paths. Host fallback is an implementation choice, never
runtime fallback from an unavailable remote workspace to a local database.

Stage 1 is complete only when every ledger row has reproducible evidence for the
selected shape and remaining production capacity questions are explicit. Update the
canonical host choice/rationale, retain only the selected host's useful harness, and
stop for review before touching production storage. Report blockers honestly; an
unproven gate means stage 1 is incomplete.

## Verification and handoff

Use repository-required checks for any repository code changes: `cargo fmt --check`,
`cargo test --all-targets`, `cargo test --doc`, and
`cargo clippy --all-targets --all-features -- -D warnings`. Run the isolated lab's own
Rust/Wasm, JS/TS and actual workerd checks too; root Cargo checks do not cover it.

End with the selected host or blocker, evidence report path, pinned versions,
commands/results, measured costs, outstanding risks and JJ/push/deployment status.
Preserve all four reviewed contracts in the design: original principal-bound recovery,
reviewed-authority submission, complete legacy-writer/schema fencing, and fresh
suspend-aware server-time anchoring. Their product tests remain later-stage work.
