# Terminal time tracker

**tt** tracks work under hourly agreements and monthly retainers, with a keyboard-first TUI and a CLI. See running timers, daily earnings, and monthly estimates. For macOS and Linux.

> Early development: schema changes currently require a fresh database. Back up your data before updating. Older databases that permit timer ID reuse are rejected; no automatic migration is provided.

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

## Worth knowing

- Each project belongs to one client. One timer may run per client; different clients can overlap.
- Pricing changes create replacement agreements; historical entries keep their original terms. Project client ownership and billing timezone are fixed.
- Retainer included hours aren't a recording cap. An explicit monthly maximum limits recording; stopping an over-cap timer discards the overflow.
- Earnings are estimates, not invoices. Currencies stay separate.

## TODO

- [ ] Browse completed-entry history per agreement.
- [ ] Delete entries with confirmation.
- [ ] Edit completed entries.

## Development

Use `cargo run` to try local changes. Before committing:

```sh
cargo fmt --check
cargo test --all-targets
cargo test --doc
cargo clippy --all-targets --all-features -- -D warnings
```

Review intentional TUI snapshot changes with `cargo insta review`; keep `.snap` files, not `.snap.new` files.
