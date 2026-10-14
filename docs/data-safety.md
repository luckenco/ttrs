# Native data safety (stage 3)

Stage 3 provides library capabilities, not the proposed `tt backup`/`tt restore`
CLI commands or remote storage. Public contracts live in `tt::maintenance` and
`tt::portable`; the import example is a compiled, executed Rustdoc test.

## Opening and migration

`Tracker::open` still creates a missing workspace. Maintenance backup/export require
an existing source, so a mistyped source cannot silently export an empty workspace.
Both can upgrade a supported legacy source.

The supported legacy schema is the exact pre-stage-3 schema in `schema/legacy.sql`,
including AUTOINCREMENT timers/entries and retainer columns. Unknown/newer schemas,
extra schema objects and earlier schemas fail closed. Pre-AUTOINCREMENT databases
cannot recover deleted-ID high-water marks; do not invent those marks or reset the
file. Preserve it for explicit, separately reviewed recovery.

Migration reserves the SQLite writer lock, captures a consistent safety copy via a
second reader, verifies integrity, foreign keys and fallible domain/sequence values,
then fsyncs and publishes `<db>.pre-migration-UUID.sqlite`. Only then does one atomic
transaction rename **all five** domain tables to `tt_*`, install versioned metadata,
create a new workspace/incarnation and install the empty `clients` sentinel.
All legacy sentinel writes abort, other legacy names disappear, and cached old
statements fail too. Fresh/imported/restored databases install the same fence.
Failure rolls back schema changes and fencing; a published safety copy is retained.

Native handles hold shared file maintenance leases. Tracking mutations increment
revision in the same IMMEDIATE transaction; errors/unwind do not advance it.
Application expected-authority checks and receipts remain stage 4 work.

## SQLite and portable backups

SQLite backups use the online backup API, never a live WAL main-file copy. The
standalone copy is verified, closed, fsynced, and published atomically without
clobbering an existing output; its directory is then fsynced. `maintenance::export`
reads an independent captured copy, not changing live tables.

The first open per incarnation/UTC day creates and verifies
`<canonical-db>.daily-YYYY-MM-DD-INCARNATION.sqlite`. Subsequent opens verify that
copy without overwriting it; concurrent first opens use no-clobber publication.
Backup failures are reported as open errors. This is a beginning-of-day-on-use
copy, not continuous protection or an off-device schedule. No automatic deletion
is implemented: monitor disk usage and retain independent copies. UI freshness
reporting/retention policy remains later frontend/production work.

Portable format 1 is compact JSON with a manifest, data section and empty receipt
section. Wide values are canonical decimal **strings**, never JSON numbers. The
schema fixes table and column order in `src/schema.rs`; rows sort by positive ID.

- Data: all clients, projects, agreements, running timers and completed entries;
  timer/entry sequence high-water marks, including deleted maxima; source revision.
- Manifest: format/schema/rules versions, source workspace/incarnation, revision,
  capture time, counts, data size and SHA-256 data/receipt digests.
- Units: UTC timestamp microseconds, whole monthly hours, currency millionths,
  ISO dates and named IANA billing timezones.
- Canonical data bytes: compact serde JSON, field order `tables`, `sequences`,
  `revision`. Data digest excludes identity/capture provenance so import under
  a new identity can be compared exactly.
- Receipts: exactly `[]`, because no operation service/receipt schema exists yet.
  Future receipt support requires a new format; format 1 rejects unknown fields
  and nonempty receipt sections instead of silently dropping them.

Verification is fallible, not deserialization into asserting storage constructors.
It checks counts/digests, canonical integers, ranges, nullability, IDs, names,
pricing/currency/limits, dates/zones, foreign references, agreement succession and
work periods, single-client timer ownership and sequence high-water marks.
It does not replay command admission or recalculate every historical monthly cap.
Checksums detect corruption, **not authorship**; restore only trusted snapshots.
No credentials, grants or frontend preferences are exported.

## Empty import and native restore drill

`maintenance::import` requires a nonexistent target and no stale WAL/SHM sidecars.
It builds a temporary empty database, creates fresh workspace/incarnation IDs,
inserts explicit IDs in foreign-key order, restores sequences/revision and compares
the imported data digest before committing/publishing. Failed imports leave the
target untouched. Numeric IDs remain stable; authority identity does not.

`maintenance::restore` is an explicit destructive library call: invoking it confirms
loss of target work not present in the supplied verified snapshot. It requires
exclusive maintenance access (all upgraded handles closed), preserves the target
workspace and rotates its incarnation. Supported legacy targets first migrate with
a safety copy. The final old cut gets a verified `<db>.pre-restore-UUID.sqlite`
before SQLite checkpoints, exits WAL mode, closes and atomically replaces the file.
Running timers remain running. Restoring from another workspace is allowed by this
low-level capability; it never installs the source's identity.

Both paths retain the legacy sentinel and verify imported bytes before publication.
File/directory fsync errors after publication can report failure even though a valid
output is installed: inspect it before retrying. Replacement files are private
(0600 on Unix). Restore refuses symlink/hard-link targets. Manual replacement of a
live SQLite file and raw handles during maintenance are unsupported.

## Limits and remaining work

- Full native integrity/domain verification and portable capture materialize history;
  memory is O(snapshot), with additional serialized buffers. Large local workspaces
  are not truncated or arbitrarily capped, but can exhaust RAM or delay opening.
  Remote admission budgets and immutable staging are later work.
- Backups are retained indefinitely and can fill disk; backup failure blocks opening
  or migration rather than silently proceeding unprotected.
- Restore requires a verifiable target safety copy. Corrupt/unsupported-target salvage
  is not implemented; preserve the original files for a separately reviewed recovery.
- Filesystem locking, atomic rename and directory fsync assume a supported local
  filesystem. Network filesystems, power-cut fault injection and Linux runtime
  execution have not been proven by the macOS tests.
- Source provenance is not authenticated and timezone interpretation uses the bundled
  Jiff tzdb. Native/workerd business parity, remote receipt-aware snapshots,
  reviewed-authority service checks, confirmed frozen loss reports, CLI/TUI maintenance
  commands, off-device scheduling and production restore fault matrices remain later stages.

Reproducible checks and coverage are recorded in the
[stage-3 evidence](remote-workspace.md#stage-3-data-safety-evidence).
