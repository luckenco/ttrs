# Data safety: native and connected workspaces

Public library contracts live in `tt::maintenance` and `tt::portable`; Rustdoc
examples are executable doctests. See [workspace invariants](worker-contract.md)
and [connected operation](remote-runbook.md) for the remote boundary and assumptions.

## Opening and migration

`Tracker::open` creates a missing native workspace. Maintenance backup/export require
an existing source: a mistyped path must not export an empty workspace. Selecting
remote occurs before native path resolution; remote failures never create a local
fallback database. Remote ordinary access to unknown names returns 404 without
tracking-table initialization; only explicit admin `tt workspace create` bootstraps.

Supported legacy schemas are recognized exactly, including AUTOINCREMENT sequence
history. Unknown/newer schemas, extra application objects and pre-AUTOINCREMENT
history without recoverable deleted-ID high-water marks fail closed. Preserve the
original files; do not invent high-water marks or reset them.

Legacy migration reserves the SQLite writer lock, captures a consistent online
safety copy through a second reader, verifies integrity, foreign keys and fallible
domain/sequence values, then fsyncs and publishes `<db>.pre-migration-UUID.sqlite`.
Only afterward does an atomic transaction rename all five domain tables to `tt_*`,
install metadata/identity and the empty write-rejecting legacy `clients` sentinel.
Schema-v1 to current schema-v2 migration likewise publishes a verified safety copy
**before** atomic schema change. Fresh/imported/restored databases retain the sentinel;
cached legacy writes fail too. Transaction failure rolls back schema changes, not
an already published safety copy. No live v1 migration RPC is implemented in the
Worker: older routed schemas fail closed; use verified import into a fresh target.

Native handles hold shared maintenance leases. Domain writes increment revision in
the same transaction. Workspace mutations additionally check reviewed authority and
full captured timer/entry/project values; successful requests atomically persist
work, revision and receipt. Rejections are recorded only after domain rollback, in
a separate receipt-only transaction. Infrastructure failures never become rejection
receipts. Receipt replay does not advance revision.

## SQLite backup and publication

Native backup uses SQLite's online backup API, never a live WAL main-file copy.
The standalone copy is verified, closed, fsynced and published no-clobber; its
parent directory is then fsynced. Portable export reads an independent captured
copy, not changing live tables.

First open per incarnation/UTC day creates and verifies
`<canonical-db>.daily-YYYY-MM-DD-INCARNATION.sqlite`; subsequent opens verify rather
than overwrite it. Concurrent first opens publish no-clobber. Failure blocks open.
This is beginning-of-day-on-use, not continuous/off-device protection. No automatic
native backup deletion exists: monitor disk and arrange independent copies.

**Publication is not rollback:** file/directory fsync errors after publication may
report failure with a valid output already installed. Inspect before retrying.
Private replacement/download files use 0600 permissions on Unix. Filesystem leases,
hard links, atomic replacement and directory fsync assume a supported local filesystem.
Network filesystems, power cuts and Linux runtime behavior are not certified by macOS
checks. Never manually replace a live workspace or copy only its WAL database main file.

## Portable format 2 (format 1 remains readable)

Current snapshots are compact JSON format 2/schema 2. Wide values are canonical
exact decimal strings, never JSON numbers. Table/column order is fixed; rows sort by
positive ID. UTC times are microseconds, monthly hours whole hours, money currency
millionths, dates ISO and billing zones named IANA zones.

- Data includes all clients/projects/agreements, running timers/completed entries,
  revision and timer/entry high-water marks, including deleted maxima.
- Manifest includes versions, source workspace/incarnation, capture time, counts,
  sizes and SHA-256 digests. Data digest covers compact `tables,sequences,revision`
  and excludes identity/capture provenance.
- Receipt digest covers complete sorted live and source-audit receipts. Imported
  receipts retain their source authority/principal/key/envelope/outcome but are
  **audit-only**, never replay permission in a fresh target.
- Operational digest covers `[fence,fence_audit]`: meaningful source fence/floor
  proofs are retained as audit, not target activation permission. Import retains any
  receipt-referenced scope and any non-null purge floor (even without receipts),
  dropping only empty no-receipt/no-floor fences on carry-forward. Job-only history
  remains in source snapshots and durable control/R2 reports, not replay authority.
  The durable local purge floor prevents absent purged requests becoming retryable
  or NotApplied after clock correction: status remains conservatively OutcomeExpired.
- Format 1/schema 1 is still readable with exactly empty receipts and no operational
  digest. Import creates fresh identities; no source identity authorizes replay.
- Verification requires a fence or audit proof for every receipt context and rejects
  identity collisions. Fresh identity and empty-fence compaction change the operational
  digest; all data, sequences, revision and receipt digests remain unchanged. Meaningful
  receipt/floor proofs persist indefinitely and may exhaust bounded Worker capacity;
  writes/import then refuse safely rather than deleting proofs.

Verification rejects duplicate/non-increasing receipt keys before import; a valid
digest alone must not admit receipt order that canonical re-capture would change.
Verification is fallible and checks digests/counts, canonical integers, ranges,
nullability, IDs/names, currency/pricing/limits, dates/zones, references/succession,
work periods, timer ownership and high-water marks. It does not replay admission
or recalculate every historical monthly cap. Checksums detect corruption, **not
source authorship**. Restore only trusted snapshots. Credentials, grants, frontend
preferences and remote routing/control jobs are not exported.

## Native import and low-level restore

`maintenance::import` requires a nonexistent target without stale WAL/SHM sidecars.
It builds/verifies a temporary database with fresh workspace/incarnation, inserts
explicit IDs in foreign-key order and preserves sequences/revision/data and receipt
digests before publication. Twenty idle native import generations retain zero empty
fence-audit rows rather than accumulating meaningless identities. Running timers
remain running.

`maintenance::restore` is explicitly destructive, requires exclusive maintenance
access (all upgraded handles closed), preserves target workspace and rotates its
incarnation. It retains a verified final `<db>.pre-restore-UUID.sqlite` before
checkpointing, exiting WAL, closing and atomically replacing the file. Supported
legacy targets first migrate safely. Source identity is never installed. Symlink/
hard-link targets are refused; corrupt/unsupported-target salvage is not implemented.

CLI spelling:

```sh
tt --backend local backup create --output cut.sqlite
tt --backend local backup export --output cut.json
tt backup verify cut.json                       # file-only; no backend/database open
tt --backend local restore import cut.json      # new target only
tt --backend local restore replace cut.json --yes
```

Native `replace --yes` is the exclusive **low-level restore**, not a frozen-loss-report
workflow. It has no frozen report. Confirmation is checked before file/database I/O.

## Connected maintenance

```sh
tt workspace create                             # explicit remote admin bootstrap
tt backup create                                # retained remote manifest; no --output
tt backup export --output cut.json               # create then verified download
tt backup download MANIFEST --output cut.json    # existing published manifest only
tt restore stage cut.json                        # fresh inactive target
tt restore status                               # discover durable intent after uncertainty
tt restore freeze JOB                            # drive preparation to confirm
tt restore report JOB --output frozen-report.json
tt restore activate JOB --report frozen-report.json --yes
tt restore abort JOB                             # pre-flip only
```

Downloads go through the selected authenticated Worker, not arbitrary R2 URLs.
Worker verifies manifest/payload; Rust verifies the whole-body SHA-256 and portable
semantics before no-clobber file and directory fsync. Reports use the dedicated
4 MiB whole-report route, validate header/status/file SHA-256 and bind job, old/target
authorities and frozen revision. Activation requires the **exact whole frozen report
file**, reviewed and confirmed before I/O; summaries/preliminary reports are not
accepted. Uncertain admin responses require status inspection and explicit resumption,
not a claim of rollback. Abort is unavailable after route flip.

The host implements hourly alarms, daily/monthly selection, verified immutable
publication and resumable frozen restore. It never deletes R2 objects or configures
locks/lifecycle. Personal backups remain in-account; account-wide loss is a shared
risk. Full capture is O(all rows/receipts); byte limits do not certify CPU/memory.
Control records remain indefinitely, with a monitoring-only 50 MB SQLite warning.
