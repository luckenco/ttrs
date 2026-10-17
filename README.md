# Terminal time tracker

**tt** tracks work under hourly agreements and monthly retainers, with a keyboard-first TUI and a CLI. See running timers, daily earnings, and monthly estimates. For macOS and Linux.

> Supported legacy databases migrate automatically only after a verified safety backup. Unknown/newer schemas and older databases without recoverable deleted-ID high-water marks are left unchanged—preserve them, do not reset them. Native opens also create verified daily-on-use backups alongside the database.

## Get started

With Rust and Cargo installed, run from this repository:

```sh
cargo install --path . --locked
tt
```

Make sure `~/.cargo/bin` is on your `PATH`. Rerun the install command to update.

In an empty workspace, press **n** to create a client, then **: → New project + initial agreement** to set up your first agreement.

## Using the TUI

**Dashboard** and **Agreements** are the main screens. Clients, Projects, and Settings are available through the `:` action menu—no nested navigation required.

| Key | Action |
| --- | --- |
| `1` / `2` | Dashboard / Agreements |
| `s` | Start or switch a timer |
| `x` / `X` | On Dashboard: stop now / choose a stop time |
| `l` | Log completed time against the selected agreement |
| `/` | Search the current list |
| `:` | Find actions and administrative screens |
| `?` / `F1` | Workspace help / form help |
| `q` | Quit; timers keep running |

Forms use `Tab` to move between fields, `Enter` to choose or review, and `Esc` to go back. Administrative changes require confirmation; the quick timer picker starts on `Enter`. Small terminal splits work down to **32×10**.

**Need to undo a mistake?** On Dashboard, select a timer and use **: → Cancel selected timer**; confirmation removes it without recording completed time. In Agreements, select an agreement and use **: → Browse completed entries**. Search the list, press `Tab` for full details, or `Enter` to review deletion and `Enter` again to confirm. `Esc` cancels without changing work. Historical and archived agreements are available with `a`. Deletion is permanent and updates recorded usage and hourly/overage estimates; it does not remove a retainer’s fixed fee.

**Forgot to track something?** Open Agreements (`2`), select the agreement, and press `l`. Enter a scope (ticket or description) and From/To times, then review and save. Historical agreements are available with `a`. Stop that client's running timer before logging.

Times like `0900` or `09:00` mean **today** in the display timezone. For past or overnight work, use explicit dates such as `2026-03-12 09:00`. Offset timestamps are also accepted. Change the display timezone in Settings; otherwise your device timezone is used.

## Prefer the CLI?

Once you've created a project:

```sh
tt start -p website -s ISSUE-123
tt status
tt stop --id id:1
tt log -p website -s "Planning" --from "2026-03-12 09:00" --to "2026-03-12 10:00"
tt --help
```

Use `tt <command> --help` for all options, including client/project setup, replacement agreements, archival, and timezone configuration.

Recovery commands require explicit confirmation:

```sh
tt cancel --id id:1 --yes                  # Cancel without recording an entry
tt entry list --agreement id:1            # Includes historical/future entries
tt entry delete --id id:1 --yes            # Permanently delete a completed entry
```

Timer IDs and entry IDs are separate, non-reusable identifiers. Inspect `tt status` or `tt entry list` before confirming recovery.

## Worth knowing

- Each project belongs to one client. One timer may run per client; different clients can overlap.
- Pricing changes create replacement agreements; historical entries keep their original terms. Project client ownership and billing timezone are fixed.
- Retainer included hours aren't a recording cap. Monthly maxima are pooled per project and prorated by calendar days of agreement coverage. **Any uncapped agreement coverage makes that entire calendar month uncapped**, including days governed by capped terms. For example, replacing uncapped terms halfway through April with a 20-hour maximum leaves all of April uncapped; May has the full 20-hour maximum. Stopping an over-cap timer discards the overflow.
- Earnings are estimates, not invoices. Currencies stay separate.

## TODO

- [x] Browse completed-entry history per agreement.
- [x] Delete entries with confirmation.
- [ ] Edit completed entries.

## Data safety

The Rust library provides `tt::maintenance::{backup, export, save, import, restore}`
and `tt::portable::verify`. Consistent SQLite backups and portable snapshots preserve
all tracked work, running timers, revision and deleted-ID high-water marks. Imports
create a new identity; restores require closed native handles, save a final safety
copy and rotate the incarnation. **Restore is destructive** and never silently stops
running timers.

See [native data safety](docs/data-safety.md) and the compiled `maintenance` Rustdoc
example for usage and supported schemas. CLI maintenance is implemented:

```sh
tt --backend local backup create --output cut.sqlite
tt backup export --output cut.json
tt backup verify cut.json
tt --backend local restore import cut.json      # nonexistent target; new identity
tt --backend local restore replace cut.json --yes # exclusive low-level restore
```

Connected mode uses explicit `tt workspace create`, remote `backup create` without
`--output`, verified manifest downloads and `restore stage` / `freeze` / `report`
/ `activate JOB --report FILE --yes`. Native replace has no frozen loss report;
remote activation requires the exact whole frozen report. See the
[operator runbook](docs/remote-runbook.md) before using connected maintenance.

Native daily/safety copies remain on-device and are not automatically deleted: monitor
disk usage and arrange independent copies. Never copy only a live WAL database's
main file or manually replace a live workspace.

## Connected mode

After configuring a remote workspace, just run `tt` or `tt status`.
Tracking commands are unchanged. There is no offline write queue or local fallback;
switching backends does not move data. Use `tt --backend local` to access local data.

### CLI authentication

On another machine, install `tt` and securely transfer an app token file to
`~/.config/tt/remote-token`, then:

```sh
chmod 600 ~/.config/tt/remote-token
tt config backend remote \
    --url https://tt.example \
    --workspace personal \
    --credential-file ~/.config/tt/remote-token
tt status
```

Settings in `~/.config/tt/backend.json` store the URL, workspace and credential-file
reference, not the token. The CLI reads the referenced token file and authenticates
with an HTTPS bearer token, checked against the Worker's `GRANTS` secret.
No `cf auth login` is needed to use `tt`; management credentials are separate. Alternatively,
use `--credential-env NAME` to reference an environment variable.

Use a write token for tracking, an admin token for backup/restore, and a read token
for MCP. Never put token values in command arguments, Git or chat.

### Storage and Cloudflare dashboard

Select your deployment's Cloudflare account. Dashboard access uses your Cloudflare login:

- **[Durable Objects](https://dash.cloudflare.com/?to=/:account/workers/durable-objects)
  → `ttrs-connected_WorkspaceDO` → Data Studio:** live SQLite `tt_*` tables.
  Enter the current incarnation UUID as the object **name**, not `personal`; find it
  in a current backup's `manifest.incarnation`. It changes after restore.
  **Inspect only:** direct SQL edits bypass validation, revisions and receipts.
- **R2 → `ttrs-backups`:** private JSON snapshots and manifests under
  `hourly/`, `daily/` and `monthly/`. Backups remain in the same account.
- **Workers & Pages → `ttrs-connected` → Observability:** logs and traces.

Connected use is personal-scale; do a practical restore check before trusting real data.
See [invariants and assumptions](docs/worker-contract.md), the
[operation runbook](docs/remote-runbook.md) and [Worker setup](worker/README.md).

## Development

Use `cargo run` to try local changes. Before committing:

```sh
cargo fmt --check
cargo test --all-targets
cargo test --doc
cargo clippy --all-targets --all-features -- -D warnings
```

Review intentional TUI snapshot changes with `cargo insta review`; keep `.snap` files, not `.snap.new` files.
