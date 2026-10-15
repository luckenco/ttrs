# Connected Worker

Rust owns domain rules, SQL, receipts and portable verification. This package owns
Cloudflare authentication, SQLite DO transactions/routing, R2 and alarms.
See [invariants and assumptions](../docs/worker-contract.md) before changing boundaries.

## Local checks

Use Node/npm versions from `package.json`, Rust 1.98.1 and wasm32-unknown-unknown:

```sh
cd worker
npm ci
npm run types
npm run check
WRANGLER_DOCKER_BIN=/nonexistent npm run build
npm test
```

The Docker override skips an irrelevant daemon probe in the pinned container-free
Vite build. Use cf, not a Wrangler replacement. Compatibility date 2026-10-06 matches
the pinned workerd; upgrade runtime/date together and rerun checks.

Tests load the cf artifact unchanged into local Miniflare/workerd and use throwaway
credentials/storage. Fault/trap sidecars are not shipped. The Rust CLI fixture's
self-signed loopback TLS exception does not change production certificate validation.
Generated types, builds, local state and results stay ignored.

## Configuration

Review names/bindings/ORIGIN in `cloudflare.config.ts` before an approved deployment.
The placeholder origin is non-routable. Use one exact HTTPS origin.

- GRANTS: secret `{"grants":[{"principal":"stable-id","role":"read|write|admin","token":"..."}]}`.
  Generate high-entropy tokens (at least 128 random bits); preserve principal on rotation.
- CONTROL: a separate high-entropy private-RPC secret, never a client credential.
- BACKUPS: the R2 bucket. WORKSPACE/AUTHORITY: the two SQLite DO namespaces.

Grants apply across **all workspace names**, not per-workspace ACLs. App credentials
are never Cloudflare management tokens. The app provisions no backup locks/lifecycle,
external alerts or automatic deletion. See the [operation runbook](../docs/remote-runbook.md).

## HTTP contract

Paths below are under `/v1/workspaces/{name}/`. Require bearer auth and exact
origin/Host; authenticate before routing/body reads. Success uses root protocol
values, not host wrappers. Errors are `{"error":"StableCode"}` with non-2xx status.
All responses are no-store. Unknown names do not bootstrap.

| Method | Path | Body / result |
| --- | --- | --- |
| GET | session | root Session |
| POST | read | root Query → Projection |
| POST | commands | root Envelope → RecordedOutcome |
| POST | requests/status | original Envelope → Resolution |
| POST | admin/bootstrap | {} |
| POST | admin/backup | {} |
| POST | admin/backup/status, admin/backup/schedule | {} |
| POST | admin/backup/download | {manifest}; verified binary snapshot, x-backup-sha256 |
| POST | admin/import | complete portable snapshot, ≤2 MiB |
| POST | admin/restore/stage | {manifest} |
| POST | admin/restore/status | {job} for current/historical ID, or {} for current/null |
| POST | admin/restore/advance, admin/restore/abort | {job}, current job only |
| POST | admin/restore/report | {job}, ordinary JSON response cap |
| POST | admin/restore/reportDownload | {job}; whole binary report ≤4 MiB, x-report-sha256 |
| POST | admin/restore/accept | {job, revision, digest, confirmed:true} |

Verify the entire downloaded report against its header and status digest before
confirmation. Oversized reports fail before confirm and leave abort available.
`/mcp` uses the pinned SDK, read-role credentials only, and exposes
overview/catalog/entries/scopes through the authenticated frontend; no write or SQL tools.

## Regression coverage

- `bridge.mjs` / `probes.mjs`: exact SQL, rollback/traps, disposable Wasm, Rust-owned rejection.
- `faults.mjs`: durable cuts, R2 failures/races, alarm rearm, freshness and restore fencing/history.
- `integration.mjs`: actual-artifact HTTP/auth/MCP, native parity and size-boundary refusal.
- `client.mjs`: real Rust CLI roundtrips, journal recovery and verified restore/download.
