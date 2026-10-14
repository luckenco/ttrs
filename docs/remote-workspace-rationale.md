# Rationale: selectable workspace authority

## Problem

Add connected remote tracking and agent reads without losing local SQLite mode or
creating two implementations of billing/tracking rules. Current rules are interleaved
with rusqlite transactions, the TUI performs synchronous I/O, and restoration can
rewind IDs. Non-reused IDs, selected agreements, full-value checks and coherent
status reads are explicit historical constraints (`80939d55`, `fa1b96c1`,
`be406813`, `36ddbbff`), not abstractions added for the remote feature. Backups must
preserve the complete workspace; `plugins.md` exports/invoices are still proposals.

## Usage (caller's view)

```rust,ignore
let mut workspace = workspace::open(selection, device)?;
let view = workspace.read(workspace::Query::Overview { zone })?.into_overview()?;
workspace.apply(&view.stamp.authority, workspace::Operation::Stop {
    expected: view.dashboard.timers[0].timer.clone(), at: workspace::WorkTime::Now,
})?;
// TUI submits the same operation to its effects worker; it never calls HTTP on input.
```

Users select local or remote once, then retain tracking command syntax. Agents use
read-only MCP against the selected remote authority. Existing local `Tracker`
methods/doctests remain valid. See the [usage and full design](remote-workspace.md)
and [type sketch](remote-workspace-sketch.md).

## Shape

One Rust rule/SQL source runs over a private synchronous text/null SQL capability,
natively on rusqlite and on normal Wasm behind the selected thin TS DO host.
Stage 1 tried workers-rs first and selected TS after the lifetime gate. Hosts own transactions;
Rust owns cross-row rules and exact integer arithmetic. This hides persistence
variation without a repository method per business operation, per boundary-discipline
and laziness-protocol. Text-cast SQL and rejecting numeric result cells encode the
JavaScript precision lesson structurally, per encode-lessons-in-structure.

One canonical Workspace owns shared timers/caps; separating actors' tracked data
would violate the real coordination invariant. Authority DO owns only routing and
restore jobs. Fresh incarnations plus freeze-before-cutover fence restored IDs,
per separate-before-serializing-shared-state. Application operations hide selection,
protocol and receipts but expose relevant preconditions and unknown outcomes;
pretending uncertainty is an ordinary failure would not be a deep interface.
Per-request journals and bounded receipt admission protect crash/retry safety,
per make-operations-idempotent. No writable replica, offline queue or invoice effects.

## Synthesis decision

Three isolated candidates were produced and read in full:

- **A, Sonnet:** shared transactional Rust/SQL over a small synchronous adapter.
- **B, Astra:** complete workspace facts -> pure decision engine -> persistence plan.
- **C, GLM:** all-Rust workers-rs DO and local/remote Tracker facade.

Base **A** preserves existing selective queries and transaction sequencing with less
behavioral rewrite than B. B loads all history per command and adds a hard growth
ceiling; C assumes an incorrect transactionSync callback, restricts valid i64 values,
lacks a durable client journal, and proposes copying a live WAL file. Those are
load-bearing red flags, not acceptable implementation details.

Parent rubric (1–5):

| Criterion | A | B | C |
| --- | --- | --- | --- |
| Shared rules / existing invariants | 5 | 4 | 3 |
| Atomicity, retries, restoration | 4 | 4 | 2 |
| Interface depth / information hiding | 4 | 3 | 3 |
| Incremental cost / frontend fit | 4 | 2 | 3 |
| Runtime feasibility / exactness | 3 | 4 | 2 |
| Data safety / scaling | 4 | 4 | 2 |
| Total | 24 | 21 | 15 |

GLM cross-judged all candidates and also preferred A. It judged its own model-family's
C, so that part is not independent validation. No model's score proves feasibility.
Adapted from B: bounded admission/recovery deadlines, archive commercial-context
guards, old-binary fail-closed migration, restore freezing, and bounded TUI quit.
From C: keep the normal Tracker local/compatible, but reject the remote facade and
handwritten MCP implementation. That rejected candidate is not rehabilitated by the
Rust-first host revision below.

Parent revisions to A: per-request rather than shared journals; receipt-only
rejections after rollback; explicit host binding rather than ambient Wasm state;
no automatic crash/reconnect submission; no per-write Authority high-water RPC;
source-scoped restored receipts; and measured backup budgets rather than invented
capacity promises. Final review also bound late acknowledgements to their originating
authority and required loss acceptance against the frozen restore cut. These address
multi-process races and restore gaps. Final contracts supersede temporary candidates.

Architecture-review fixes bind each journal/envelope to its original stable principal
and carry the reviewed authority through `Workspace::apply` and queued effects.
Credentials may rotate; pending work may not move to another actor or incarnation.
The first migration fences every legacy write target, including already-open handles,
and reserves a write-rejecting `clients` sentinel to prevent legacy reinitialization.
Authenticated session bootstrap supplies request/relative-input time with bounded,
suspend-aware freshness; refreshing it never rewrites existing keys or reviewed times.
These contracts have explicit acceptance cases in the design, not implemented tests.

### Host-choice revision (2026-10-13)

The user asked to try workers-rs rather than assume a TS host. This changes platform
ownership, not the shared rules, application interface, or safety contracts above.
Two new isolated proposals (Sonnet 5.5 and GPT-6 Astra) compared whole-host shapes:
Rust Worker + Rust DOs, TS Worker + TS DOs around the Rust core, and TS ingress +
Rust DOs. Both prefer conditional Rust-first with the complete TS host as fallback.

Base: Astra's proposal, for its capability/proof ledger and explicit cf packaging
and runtime-lifetime gates. Independent GLM cross-judgment preferred it (20/20 vs
19/20 on safety, runtime realism, boundary depth and handoff clarity). The parent
agreed on the base; these scores do not prove runtime feasibility. Grafted from
Sonnet: a negative-control test showing that returning Err as callback data commits,
and named local probes for exactness, rollback and isolation. Rejected: unnecessary
new core-crate scaffolding, silent tooling substitution and a custom MCP protocol.
No candidates failed. The original scoreboard remains historical, not a verdict on
this new Rust-host design.

The initial preference was one Rust platform host that links the shared core. Optional TS adapts an
established read-only MCP SDK over authenticated private service access; it does
not own transactions. If Rust needs broad binding work, SDK forks or repeated
artifact patching, use the TS platform host instead. TS ingress with Rust DOs is
not the automatic compromise: it retains the hard Rust transaction/export seams.
The [stage-1 handoff](remote-workspace-handoff.md) required selecting one host after
real workerd proofs. That experiment is complete; see the decision below.

### Stage-1 evidence status (2026-10-13)

[The isolated lab](../spikes/remote-host/evidence.md) proves cf packaging, Rust/Jiff
on normal Wasm, exact text/null SQL, synchronous rollback and the optional SDK
read adapter in local workerd. One narrow transactionSync binding works, including
Result-to-exception translation after Rust destructors run. No SDK fork, generated
artifact patch or Wrangler substitution was needed.

The pinned shared Wasm glue replaces its instance after a trap, but both the
trapping request and a sibling Rust SDK future held across that reset remain
unsettled at the lab's bound. Fresh calls and SQL isolation work; that is not proof
of safe in-flight recovery. The thin TS-host experiment uses per-DO, disposable
synchronous Rust instances and now passes the complete local feasibility ledger:
exact row-returning SQL, rollback and receipt precedence, both held-sibling
schedules, same-DO stale-capability refusal, real eviction/restart, constant-time
bounded auth and private SDK MCP access, manifest-last R2, persisted alarm retries,
and capture/crossing/heap costs. These remain synthetic proofs, not business parity
or production capacity certification.

**Decision: select TS Worker + TS SQLite DOs + synchronous Rust Wasm core.
Stage 1 is complete; stop for review before stage 2.** The losing capability
harness was removed; only its small, still-failing shared-runtime trap repro is
retained as evidence for this choice. The selected host needs no SDK fork,
artifact surgery or ambient mutable SQL router. This changes no rule ownership,
full-range integer guarantees, principal/authority recovery contracts, legacy-writer
fencing, suspend-aware server-time anchoring or complete-backup requirements.

### Native-seam follow-on (2026-10-14)

The user approved stage 2 after review. [The native shared SQL seam](remote-workspace.md#stage-2-native-seam-evidence)
is complete: existing rule/query code uses exact text/null capabilities, native
transactions remain host-owned, and default Tracker behavior and schema are preserved.
Review hardened transaction ownership with SQLite's authorizer rather than relying
only on statement readonly flags. Core-only Wasm compiles without native dependencies;
actual native/workerd business parity and the remote product remain later-stage work.
Stage 3 subsequently completed native data safety; see below.

### Data-safety follow-on (2026-10-14)

The user requested stage 3. [Migrations and portable snapshots](remote-workspace.md#stage-3-data-safety-evidence)
are complete as native library capabilities: verified safety copies before atomic
legacy fencing, daily-on-use standalone backups, exact row/sequence/revision export,
empty-target import and exclusive-lease restore under a fresh incarnation. Invalid
schemas/backups fail closed; supported databases no longer need a reset. Format 1
receipts are explicitly empty until stage 4 introduces the operation service.

The native restore drill retains the final discarded work, but is a destructive
low-level call rather than the future frozen-report activation protocol. Full-history
memory, indefinitely retained on-device copies, off-device operations and platform
fault testing remain explicit limits in [the native contracts](data-safety.md).

## Tradeoffs accepted

- We accept a bounded Rust-host spike, and one small binding if proven, for less host-language glue.
- We accept optional TS MCP adaptation or TS-host fallback instead of an "all Rust" constraint.
- We accept text-cast query changes and underlying Rust/JS crossings for one exact rule source.
- We accept a narrow backend API and background effects for responsive terminal input.
- We accept a control-plane DO and brief restore maintenance for identity fencing.
- We accept bounded recovery windows and a request journal, not indefinite exactly-once.
- We accept synchronous bounded backup capture initially; growth requires staging redesign.

## Alternatives considered

- **Pure facts/plan engine:** hides SQL better but exposes completeness and O(history)
  costs to every adapter; larger rewrite of already-tested rules.
- **Rust Worker + Rust DOs:** tried first; rejected at the pinned shared-runtime
  in-flight trap-recovery gate. Reconsider only with new reproducible lifetime evidence.
- **TS Worker + TS DOs + Rust core:** selected after the full stage-1 ledger passed.
  Same domain surface; explicit per-DO Wasm lifecycle and a narrow synchronous ABI.
- **TS ingress + Rust DOs:** retains Rust transaction/packaging risk while adding a
  permanent ingress boundary; consider only if evidence isolates an ingress problem.
- **Worker + D1:** still viable; D1 has atomic facilities. It does not fail for lack
  of transactions. DO is chosen for colocated workspace commands/state, not because
  D1 cannot be correct. Re-evaluate if the chosen host fails its feasibility gate.
- **TypeScript rule port:** easy deployment, but two authoritative rule sources.
- **Offline replication now:** hides unavailable connectivity by introducing conflicts
  the current cap/coverage rules cannot merge safely. Explicitly deferred by user.

## Open questions and risks

- The pinned Rust host's in-flight trap recovery remains blocked; the selected host
  avoids shared Rust futures. Future SDK/runtime upgrades must repeat lifetime proofs.
- The selected cf-built shape passes local exactness, timezone, SDK and cost gates.
  Existing rules now use the shared SQL seam; native/Wasm runtime business parity
  remains unproven until the product core is attached to the remote host.
- Per-live-DO Wasm memory grows with active identities. Representative histories,
  receipt growth and calendar-span admission need measured production headroom.
- Are proposed 10-minute admission, 35-day receipts and hourly backups suitable,
  and who operates independent copies and restore drills?
- Which measured workspace/snapshot budgets fit the real tracking history?
- Where is the existing Lexware adapter, for a later separately scoped integration?

Source-control rationale was investigated; GitHub has no PRs. Linear `ttrs` had no
hits; `time tracker` returned unrelated first-page results, not an exhaustive search.
No external docs/chat/observability/error/analytics MCP was available. Remote ownership
and recovery policies are design choices from user requirements, not historical facts.

## Next implementation step

Review the [completed data safety and checks](remote-workspace.md#stage-3-data-safety-evidence)
and the [stage-1 host limits](../spikes/remote-host/evidence.md). Stage 4 introduces
coherent operations, reviewed-authority guards, canonical requests/receipts, journals
and frontend effects. Extend portable snapshots for retained receipt state before
using that service with real data. Remote authority and runtime business parity remain
later stages; the synthetic spike ABI is not the final product protocol.
