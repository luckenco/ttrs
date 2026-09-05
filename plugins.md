# Export plugins

Status: proposed implementation plan; commands and paths below describe the target, not existing functionality.

## Scope and decision

An export covers **one agreement and one billing month**, using the project's billing timezone. Users invoke an installed exporter explicitly to generate an XLSX file or create a Lexware invoice. Multi-agreement exports, scheduling, event hooks, and TUI extensions are outside the first version.

Use standalone executables communicating through versioned JSON. Keep the runner in Rust; plugins may use any language. Official plugins can be separate Rust packages, installed without rebuilding `tt`. A shared Cargo workspace is optional, not a prerequisite.

This follows [mdBook's renderer model](https://rust-lang.github.io/mdBook/for_developers/backends.html): the host prepares domain data and sends it to a configured executable. A structured result is a ttrs addition, useful for file paths and invoice references.

[Zed-style WASM extensions](https://zed.dev/docs/extensions/developing-extensions) offer portable artifacts and host-controlled access, but require a runtime and host interfaces. Executables keep spreadsheet libraries, HTTPS, and credential handling straightforward. They run with the user's privileges: installation implies trust, not sandboxing. Reconsider WASM if restricted third-party execution becomes a requirement. Native Rust dynamic linking adds ABI compatibility concerns without a benefit for these command-driven exports.

## Implementation sequence

### 1. Expose an agreement export

Add a documented `Tracker::export_agreement(agreement, month)` API and a JSON output path for inspecting its result independently of plugins.

Proposed code boundaries:

| Path | Responsibility |
| --- | --- |
| `src/export.rs` | Export data and versioned wire contracts, separate from internal model serialization |
| `src/store/export.rs` | Consistent read of agreement context, coverage, and intersecting entries |
| `src/tracker.rs` | Public export operation and domain validation |
| `src/lib.rs` | Publish the export module |
| `Cargo.toml` | Add `serde` and `serde_json` |

The read-only operation needs no schema change. Use the existing agreement/time indexes. Finish the database snapshot before invoking external code.

**Complete when:** deterministic fixtures cover month boundaries, DST, replacement agreements, archival, cross-boundary entries, empty retainers, and running-timer handling; the public API has a passing doctest and its JSON representation is documented.

### 2. Run configured exporters and implement XLSX

Add `src/cli/plugins.rs` for configuration and process execution. Wire the command through `src/cli/clap.rs`, `src/cli/mod.rs`, and `src/cli/app.rs`. Keep process management outside `Tracker` and leave the TUI unchanged.

Exporter names are configuration keys, not a Rust enum containing known plugins. Add `plugins.toml` beside the existing timezone configuration; preserve the timezone file and behavior.

```toml
[plugins.xlsx]
command = "/path/to/tt-export-xlsx"

[plugins.lexware]
command = "/path/to/tt-export-lexware"
```

Allow a separate argument list for interpreter-backed plugins. Invoke the executable directly with `std::process::Command`, without shell evaluation. Installation initially means installing the executable or script and registering its command; a plugin package manager is unnecessary.

Target commands:

```sh
tt export xlsx --agreement id:12 --month 2026-03 --output march.xlsx
tt export lexware --agreement id:12 --month 2026-03
```

The XLSX plugin owns spreadsheet dependencies and layout. Initially it can export work and agreement context; label monetary estimates as estimates until invoice calculations exist.

**Complete when:** an independently installed XLSX plugin produces a readable workbook; process tests cover missing executables, malformed responses, unsupported versions, nonzero exits, oversized output, and timeout cleanup.

### 3. Add billing calculations and Lexware

The current dashboard and retainer totals are estimates, explicitly excluding invoice rounding and hourly minimums. Build invoice calculations in the core before enabling invoice creation. Reuse existing arithmetic only where semantics match; keep dashboard behavior covered.

Add calculated charges to the same agreement export rather than introducing a separate persisted billing-statement framework. The Lexware plugin translates those charges into provider fields and owns its HTTP dependencies, credentials, contact mappings, tax configuration, and payment terms.

**Complete when:** hourly and retainer billing rules have deterministic tests; mocked API tests cover provider translation and uncertain outcomes; a documented manual check verifies actual Lexware creation before claiming end-to-end support. Run the repository's development checks from `README.md`, including doctests and clippy.

## Agreement export contract

The snapshot contains the agreement and pricing, project and billing timezone, client, selected month, effective coverage, and completed entries. Calculated charges are added when invoicing is implemented.

- Coverage is the intersection of the billing month, agreement effective period, and project activity. Replacement ends the previous agreement's coverage; the archival day remains included.
- Exported entry intervals are clipped to coverage while retaining source entry IDs. Stored entries remain unchanged. Sum overlapping work according to existing tracking semantics.
- Include an agreement with active coverage even when it has no entries: its retainer fee may still be payable. Reject selections with no agreement coverage.
- Proposed running-timer policy: reject exports with an unfinished timer intersecting the selected coverage, rather than silently omit that work.
- Use dedicated wire types so internal model changes do not silently change the protocol. Specify timestamps, half-open interval boundaries, units, and exact monetary representations. Decimal strings avoid JavaScript integer precision loss for large values.
- Billing calculations belong to the core; file layout and provider-specific invoice fields belong to plugins. An agreement is the calculation boundary, not a universal rule that every future invoice must contain exactly one agreement.

A whole agreement-month payload assumes personal-workspace volumes. Memory and serialization costs grow with its entry count; use measured limits and clear errors initially, revisiting streaming if large histories make this material.

## Process contract

One invocation accepts one JSON request on stdin, returns one JSON result on stdout, and exits. Diagnostics go to stderr. Requests include a protocol version, export data, and plugin options. Results identify the generated artifact or external resource; error responses distinguish rejection from an unknown external outcome.

Define the initial schema and compatibility rules before implementing both sides. Unsupported versions must fail before side effects. A discovery handshake, capability registry, and SDK are unnecessary for the first exporter-only protocol.

The runner closes stdin after writing, drains output without pipe deadlocks, bounds captured data, validates results and exit status, and cleans up children on timeout or cancellation. Treat a crash, invalid response, or timeout after invocation as potentially having performed external work. **Never automatically retry an export.**

Plugins receive the export data rather than a database connection or path. This is an API boundary, not a security restriction on a trusted executable. Keep provider credentials out of the export payload and diagnostics.

## Lexware submission safety

Before implementing the adapter, verify the current [Lexware invoice API](https://developers.lexware.io/docs/#invoices-endpoint), especially currency, decimal precision, tax requirements, draft/finalized behavior, and timeout semantics.

Use draft creation by default and make finalization explicit. Define amount conversion and reconciliation rather than silently rounding away differences or converting unsupported currencies.

Persist submission tracking in the core before external creation: source database identity, agreement-period, provider account, the exact submitted snapshot or a reproducible reference, submission state, and returned invoice ID. Serialize competing attempts for the same export target. Changed entries must not silently authorize a second invoice for an already-submitted period.

A local record prevents ordinary duplicate submissions but cannot guarantee exactly-once remote creation. The documented API does not promise invoice-creation idempotency, and a timeout can follow successful creation. Preserve an unknown-outcome state until reconciliation establishes whether an invoice exists; cancellation does not imply rollback.

Resolve remaining billing policy before stage 3: the order of minimums and rounding, retainer overage rounding, final monetary precision, and treatment of subsequent backfills. Record these rules with calculation tests rather than letting each plugin choose independently.
