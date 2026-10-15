# Connected host (local integration, not production-approved)

This package owns Cloudflare lifecycle only. The independent `tt-wasm` crate links
the real root `tt::host` without default features. All billing, SQL domain operations,
portable verification, receipts and revision changes remain Rust.

## Reproduce

Node **24.21.0**, npm **11.19.0**, Rust **1.98.1**, wasm32-unknown-unknown target:

```sh
cd worker
npm ci
npm run types
npm run check
npm run build
npm test
npm run test:capacity
```

This Worker has no containers. The pinned plugin nevertheless synchronously probes
Docker during build cleanup; an installed but unresponsive daemon can hang after
Vite reports success. For this container-free build, use
`WRANGLER_DOCKER_BIN=/nonexistent npm run build` to skip that irrelevant probe.
The latest candidate build and full tests passed with this supported environment override.

Build uses **cf 1.0.0-beta.14**, Vite plugin
**2.0.0-beta.sha-91c870c02**, and Vite **8.1.5**. No Wrangler substitution.
Tests load that cf build manifest unchanged into Miniflare
**5.20261006.1-alpha** / workerd **1.20261006.1**. Compatibility date is
**2026-10-06** because this runtime rejects 2026-10-15 as future. Upgrade the
runtime and date together, after rerunning the ledger.

Generated types, artifacts, state and results are ignored. The build does not
provision, authenticate to management APIs, or deploy. Tests use temporary SQLite
and R2 state and throwaway credentials. This Miniflare alpha overwrites Host through
its JS fetch proxy, so JS tests use loopback HTTPS with explicit Host and no pooling.
The Rust CLI fixture uses a loopback HTTP proxy forwarding transparently to self-signed
Miniflare HTTPS; `rejectUnauthorized: false` exists ONLY in that Node fixture.
Production Rust TLS validation is unchanged. Neither fixture proves external TLS
trust, deployment or independent-account/operator/human-review release gates.

Current Rust APIs and recovery semantics are documented in
[docs/worker-contract.md](../docs/worker-contract.md). Final local root/bridge/Worker
checks and the Rust CLI-to-workerd roundtrip pass; see the
[verification ledger](../docs/remote-runbook.md#8-verification-snapshot-and-remaining-work).
This remains local evidence, not an independent operator restore drill.

## HTTP contract

Success bodies are EXACT root protocol Session, Projection, RecordedOutcome and
Resolution values, not host wrappers. Errors are `{ "error": "StableCode" }` with
non-2xx status. All responses are no-store. No SQL endpoint.

| Method | Path under /v1/workspaces/{name}/ | Body |
| --- | --- | --- |
| GET | session | none |
| POST | read | root query DTO |
| POST | commands | original root Envelope |
| POST | requests/status | original root Envelope |
| POST | admin/bootstrap | {} |
| POST | admin/backup | {} |
| POST | admin/backup/status, admin/backup/schedule | {} |
| POST | admin/backup/download | {manifest: existing published R2 manifest key}; verified whole binary snapshot, x-backup-sha256 header |
| POST | admin/import | complete portable snapshot (2 MiB maximum) |
| POST | admin/restore/stage | {manifest: R2 manifest key} |
| POST | admin/restore/status | {job: UUID} or {} to discover current durable intent (null if none) |
| POST | admin/restore/advance, admin/restore/report, admin/restore/abort | {job: UUID} |
| POST | admin/restore/reportDownload | {job: UUID}; complete binary report ≤4 MiB, x-report-sha256 header |
| POST | admin/restore/accept | {job, revision, digest, confirmed: true} |

Download the entire report and verify its SHA-256 against restore/status's
reportDigest and the header before confirming. Reports exceeding 4 MiB refuse
confirm with ReportTooLarge/413 and leave abort available. The legacy JSON report
route retains the ordinary response cap.

`/mcp` uses `@modelcontextprotocol/server 2.3.1`, a per-request established SDK adapter with overview/catalog/entries/scopes
only, and read-role credentials only. It reads through the same authenticated
frontend; it is not a separately exposed authority or DO session store.

GRANTS is a secret JSON object with `grants: [{principal, role, token}]`.
CONTROL is a separate high-entropy secret for private RPC, never a client credential.
ORIGIN must be the exact HTTPS deployment origin. Grants authenticate before
routing or reading a body; every configured token hash is compared using workerd's
crypto.subtle.timingSafeEqual. Rotating tokens preserves the principal. HTTP JSON
cannot supply the role, routing object, or RPC capability. Unknown workspace access
does not create a tracking database. Only explicit admin bootstrap registers one, with a durable pending identity before
initialization so retry/restart reuses it. Grants are prototype deployment-wide
read/write/admin roles across all workspace names, **not per-workspace ACLs**.
Configured tokens accept 16–256 characters; operators must generate high-entropy
credentials rather than relying on this minimum-length validation.

## Runtime boundary

Each RPC/request instantiates fresh Wasm with SQL closures bound to that DO. There
is no ambient database, async Rust frame or retained pointer. alloc/command/free
exchange UTF-8 JSON; sql imports exchange materialized text/null cells only. Read
statements must start with SELECT before execution. Numeric results, buffer overflow,
import exceptions and traps throw through transactionSync and dispose the instance.

Only a domain attempt's token survives rollback, in THAT Wasm instance. Its actual
Rust Error, principal, envelope and sampled time are owned in Rust; a second synchronous
receipt-only transaction consumes it. TS cannot invent a rejection Error.

workerd's two reserved schema objects, _cf_KV and _cf_METADATA, are excluded ONLY
from the adapter's shared sqlite_schema inventory query; root inventory otherwise
includes them. This preserves exact recognition of every application object,
without filtering arbitrary unknown tables or rewriting domain rules.

Host metadata uses storage.kv. TS SQL changes only tt_fence, never tracking tables.
The Authority DO is per logical name, with durable opaque incarnation routing and
one restore job. No routing cache. Its CAS switches routing while old is frozen and
target inactive; retire precedes target enable. Failure between these phases causes
maintenance, not two writers. Repeated advance calls resume an idempotent phase.
Helpers and sensitive fields use actual ECMAScript `#private`. RPC probes reject
`ctxFor`, `move`, `job`, `config`, `env`, `ctx` and reserved alarm access. Wrong
acceptance job/revision/digest or false confirmation leaves state unchanged;
repeated abort of an already-aborted job is terminal and idempotent.

Backups capture synchronously, persist bounded cut chunks, upload immutable payload,
verify, publish manifest last, then verify both again. Failed jobs retain their cut
and retry on alarms. Hourly, first daily/monthly and safety prefixes have independent
payloads; bucket locks/lifecycle and off-account copies are operator-owned, not
silently configured or deleted by this application.

## Evidence and limits

`test/integration.mjs` exercises the actual cf artifact with durable restart,
native golden comparison, partial switch rollback, source-audit-only receipts,
post-stage writes in the frozen report, per-phase restart, missing final safety
payload refusal, explicit acceptance, CAS, abort boundaries, auth and SDK MCP listing.
`test/bridge.mjs` probes raw Wasm under Node SQLite, including numeric-result refusal
and Rust-owned rejection. `test/probes.mjs` uses a private capability-protected
workerd sidecar: a separate feature-built Wasm executes SQL then unreachable in the
same executing command/instance, and production Wasm rejects the probe command.
It also checks unknown schema objects and SQL exceptions after domain/receipt writes.
`test/faults.mjs` wraps real product DO/R2 effects with private persisted one-shot
faults/barriers: payload/manifest put/readback before/after failures, alarm retry,
lost freeze/CAS/enable acknowledgments and concurrent abort/freeze/flip. Enable
persists an alarm before admitting writes, then revalidates after the await; inactive
alarms rearm. A lost enable ack leaves target active with Authority still at enable
and the alarm already durable **before** any retry. Restart resumes the same target,
preserves intervening writes/alarms, and old mutations still refuse. Real R2 barriers
also prove duplicate completions cannot regress lastBackup or replace first-period
selections; capture order is durable and independent of clock/revision.
No fault flag, binding or test route is present in the cf production artifact.

`test/client.mjs` (`npm run test:client`, also in `npm test`) runs the actual Rust CLI
against the unchanged cf artifact: explicit bootstrap, existing-manifest download,
null/current/wrong-job status, native Berlin DST entry, quoted/backslash scope,
running timer continuation and deleted high-water 9007199254740996 (next native
allocation 9007199254740997). Native→DO→native data/hash/revision/sequences/receipts
match; two remote receipts become two native audit receipts. A lost command is
forwarded exactly once; rotated read-only credentials with the same principal
resolve it, another principal refuses, and terminal retry makes no command POST.
A post-stage LateClient appears in the final safety/loss report; acceptance binds
the whole file. B→C retains workspace identity, rotates incarnation and refuses the
old request with AuthorityChanged rather than retargeting. Remote TT_DB is never
created and credential strings are not serialized. Per-run artifact hashes are
transcript evidence, not stable golden hashes.

`test/capacity.mjs` (`npm run test:capacity`, separate from `npm test`) measures the
unchanged cf artifact in fresh runtimes with imported targets left inactive. Its
structured evidence distinguishes serialized bytes, per-Core requested Rust peaks,
linear capacity and HTTP wall time; CPU and isolate/JS/SQL peak memory are unmeasured.
See the [current measurement ledger](../docs/remote-runbook.md#9-independent-hardening-follow-up-2026-10-15).

Request limit 64 KiB; SQL JSON scratch 1 MiB; snapshot 2 MiB; response 2 MiB + 1 KiB.
These are conservative experimental refusal thresholds, NOT measured production
limits. An oversized operation rolls back, never truncates. Each successful command
also captures/preflights inside its transaction to refuse growth beyond a full
roundtrip, including merged future receipt audits and exact nested JSON encoding.
The ABI is bounded to 4 MiB + 64 KiB. Admission reserves finite 4 KiB metadata
headroom (8 KiB for nested ABI encoding); import/fence transitions check atomically.
The complete post-import inactive/new-job capture must retain 2 KiB import/enable
reserve, not zero. Enable only shortens the state word; capacity must not fail after CAS.
A valid external native snapshot can still exceed merged Worker admission: near-2 MiB
source plus live receipts refuses before old freeze, and requires explicit abort/resume.
Known receipt replay bypasses size admission. Abort clears its job UUID and remains
available even for legacy over-budget state. This is intentionally personal-scale and makes writes O(history/receipts)
despite indexed core operations. Entry browsing materializes every page on the
client and the Worker scans/materializes history; snapshots are O(all rows), journal
discovery O(retained records). Completed backup jobs, abort tombstones and tier-selection metadata accrue
in control KV without automatic cleanup. [Control retention requires an explicit
policy decision](../docs/remote-review-2026-10-15.md#control-kv-inventory-and-retention-decision);
tracking receipt horizons are not control-job expiry. JSON escaping can make a snapshot hit the ABI input
limit before the nominal import limit. See the [production gate/runbook](../docs/remote-runbook.md).
