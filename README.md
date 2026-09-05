# Terminal time tracker

> `tt` is the working CLI name. The final project name is still undecided.

A small terminal utility for tracking time against projects and scopes. A scope is either a ticket number or a free-form description; they are intentionally the same concept.

`tt` currently supports macOS and Linux. Native Windows is not supported.

## Installation

With Rust and Cargo installed, run from this repository:

```sh
cargo install --path . --locked
```

This builds a release binary and installs `tt` into `~/.cargo/bin` by default, so you can run it from any directory. If `tt` isn't found, add this to your shell config (`~/.zshrc` for Zsh) and open a new terminal:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

Verify the installation:

```sh
tt --help
```

After changing the code, rerun the install command to update `tt`.

## Is it for you?

`tt` is useful when all of these match how you operate:

- All tracked work is covered by hourly or retainer agreement pricing.
- Agreement terms do not change; a rate or limit change creates a new agreement.
- Every project belongs to one client and always has one current agreement.
- Every agreement belongs to one project; historical work keeps its original agreement.
- Project timezone and client ownership never change.
- Monthly limits use whole hours and are prorated by local calendar day across agreements.
- A client may have only one running timer; different clients may overlap.
- You work on macOS or Linux; native Windows is not a target.

## Concepts

- Every time entry has a service agreement and a scope.
- Every project belongs to exactly one client.
- Every agreement belongs to exactly one project.
- Projects define their client and immutable billing timezone.
- Agreements define hourly or retainer pricing, an optional monthly maximum, and an explicit effective date. Hourly agreements may also have a monthly billing minimum.
- Retainers define a fixed monthly `fee`, `included_hours`, and an optional `hourly_rate` for excess work. Included hours are not a recording cap. Fee and rate use the same currency.
- Creating a project atomically creates its initial agreement.
- Creating replacement terms marks the previous agreement as replaced.
- Agreement replacement, logging, and project archival are blocked while that client has a running timer.
- Exact durations enforce maximums; future billing reports round each agreement-month to the nearest 30 minutes, halves upward.
- All timestamps are stored in UTC and displayed in the configured timezone.

## CLI contract

### Start a timer

```console
tt start --project <project-name|id:project-id> --scope <scope> [--at <time>]
tt start -p <project> -s <scope> [--at <time>]
```

`--at` defaults to now. The project must be active and uses its current agreement. Starting is rejected when the client already has a running timer or the monthly maximum has been reached.

Examples:

```console
tt start --project website --scope ISSUE-123
tt start -p internal -s "Planning meeting" --at 0900
```

Starting a timer does not stop another timer. Different clients may have concurrent timers.

### Stop a timer

```console
tt stop --id id:<timer-id> [--at <time>]
```

`--at` defaults to now. Timer IDs are shown by `tt status` and returned by `tt start`.

### Show running timers

```console
tt status
```

Example output:

```text
ID    AGREEMENT     PROJECT    SCOPE              STARTED   ELAPSED  CAP LEFT
id:1  Support 2026  website    ISSUE-02           09:00     01:32    04:28
id:2  Retainer 2026 API        Planning meeting   10:15     00:17    -
```

For capped projects, `tt start` and `tt status` always show the exact allowance remaining. One hour or less is marked `WARNING`; zero is `LIMIT REACHED`. A capped timer remains visible until stopped. Stopping records only through the cap and reports any discarded overflow. Monthly minimums never alter tracked entries.

### Log a completed entry

```console
tt log --project <project-name|id:project-id> --scope <scope> --from <time> --to <time>
tt log -p <project> -s <scope> -f <time> -e <time>
tt log --agreement id:<agreement-id> --scope <scope> --from <time> --to <time>
```

Example:

```console
tt log -p website -s ISSUE-123 -f 0900 -e 1232
```

The end must be later than the start and the entry must fit within one agreement period. Agreement IDs allow historical backfills against replaced agreements. Logging is blocked while the client has a running timer. Completed entries may overlap one another.

### Manage clients, agreements, and projects

```console
tt client add --name <name>
tt client list

tt project add --client <client-name|id:client-id> --name <name> \
  --timezone <IANA-timezone> --agreement <agreement-name> \
  --rate <amount> --currency <code> --effective-on <YYYY-MM-DD> \
  [--minimum-hours <whole-hours>] [--maximum-hours <whole-hours>]
tt project archive --project <project> [--on <YYYY-MM-DD>]
tt project unarchive --project <project>
tt project list [--all]

tt agreement add --project <project-name|id:project-id> --name <name> \
  --rate <amount> --currency <code> --effective-on <YYYY-MM-DD> \
  [--minimum-hours <whole-hours>] [--maximum-hours <whole-hours>]
tt agreement list [--client <client-name|id:client-id>] [--all]
```

For a retainer, replace `--rate <amount>` with `--fee <amount> --included-hours <whole-hours>`. Optionally add `--hourly-rate <amount>` (`--rate` is also accepted) to charge for excess hours. `--maximum-hours` remains an independent hard cap; `--minimum-hours` cannot be used with a retainer. Fees and rates accept up to six decimal places.

For example:

```console
tt client add --name "Vertex GmbH"
tt project add --client "Vertex GmbH" --name Flyeralarm \
  --timezone Europe/Berlin --agreement Hourly \
  --rate 125 --currency EUR --effective-on 2026-03-01
tt project add --client "Vertex GmbH" --name shapeID \
  --timezone Europe/Berlin --agreement Retainer \
  --fee 5000 --included-hours 20 --currency EUR --effective-on 2026-03-01
```

At 24 hours in a full month, shapeID still costs €5,000. With an optional `--hourly-rate 125`, that same work would cost €5,500. Neither configuration stops tracking unless `--maximum-hours` is set. `tt agreement list` shows the optional rate plus `FEE` and `INCLUDED` columns; `-` means absent, not zero.

Project creation atomically creates the initial agreement and prints both IDs. Effective dates are required and cannot be future. Replacement is rejected if conflicting work already exists. Project archival defaults to today in the project timezone; the archival day remains active. Unarchiving restores continuous coverage retroactively.

### Configure the timezone

```console
tt config set timezone <IANA-timezone>
tt config get timezone
```

Example:

```console
tt config set timezone Europe/Berlin
```

When no timezone is configured, `tt` uses the device timezone.

Named timezone rules come from the IANA database bundled into the executable,
not the OS timezone database or `TZDIR`. This avoids scanning system timezone
files on startup and keeps rules consistent across machines running the same
build. Timezone-rule updates arrive through application updates; maintainers
update the bundle with `cargo update -p jiff-tzdb` and rebuild.

## Interactive workspace

Bare `tt` opens a persistent, keyboard-only [Ratatui](https://ratatui.rs/) workspace. **Dashboard, Projects, Clients, Agreements, and Settings** cover every CLI operation without leaving the TUI. It supports terminal splits down to 32 columns × 10 rows, using stacked totals and scrolling forms at smaller sizes. Explicit CLI commands remain unchanged; bare `tt` without a terminal returns a usage error.

During development, schema changes require recreating the local database. There are no migrations or backward-compatibility guarantees.

The dashboard shows running timers with their project, client, agreement, elapsed time, and monthly allowance. Above them, **worked today**, **earned today** and **this month · estimate** update once per second. The monthly estimate combines all retainer charges with hourly earnings this month, including capped live timers. Retainers also have a separate breakdown showing actual versus included hours and the fee plus any overage. Changes made through another CLI process appear automatically.

### Controls

| Key | Action |
| --- | --- |
| `1`–`5` | Dashboard, Projects, Clients, Agreements, Settings |
| `↑` / `↓` or `j` / `k` | Select a list row; scroll focused details |
| `Enter` | Open details, browse a client's projects, or activate a focused control |
| `/` | Search the current catalog; `Enter` / `Esc` returns to list navigation |
| `Tab` / `Shift-Tab` | Focus list/details, or move between form fields |
| `n` | Create an item in the current section; on an empty dashboard, begin setup |
| `a` / `f` | Toggle current/all items or filter by client in Projects and Agreements |
| `s` | Open the project-and-scope picker to start or switch |
| `l` | Log completed time, including against historical agreements |
| `x` | On Dashboard, stop the selected timer immediately at now, without confirmation |
| `X` | On Dashboard, review a specified stop time |
| `t` | On Dashboard, browse all totals and retainer estimates |
| `:` | Search actions, including administration and section navigation |
| `?` | Contextual workspace help |
| `F1` | Full field values, help, and errors inside administrative forms |
| `Esc` | Close the current layer; cancel the quick timer picker; dismiss dashboard feedback |
| `q` | Quit from workspace screens; timers keep running |
| `Ctrl-C` | Quit immediately, including from forms; unsaved drafts are discarded |

Letters and numbers are literal text inside search and text fields, not shortcuts. Action menus show why an operation is unavailable. Wide catalogs show a selection preview; `Tab` focuses its scrolling details. In a small split, focused details occupy the full body. `Enter` also opens a full-detail view. Details and reviews support arrows, `j`/`k`, `PageUp`/`PageDown`, `Home`, and `End`. Lists preserve selection by identity, along with their search, client/status filters, and scroll state across section changes and refreshes.

The picker fuzzy-searches active projects by project, client, or agreement name, ignoring case. For example, `wbst` matches `website`. Space-separated words match independently in any order; punctuation such as `!` and `^` is literal, not search syntax. Best matches appear first; ties and an empty query preserve project order. Press `Enter` to choose one, then select one of its three most recent distinct scopes or type a new scope. Recent scopes span replacement agreements. Inside text fields, letters—including `j`, `k`, and `q`—are text; use `↑`/`↓` to select suggestions. `←`/`→`, `Home`, and `End` move the cursor; `Backspace` and `Delete` remove characters, including whole Unicode graphemes. `Ctrl-U` clears the field. Bracketed paste inserts literal text without triggering shortcuts or submitting forms; pasted line breaks and tabs become spaces. Shortcut hints follow the current screen.

Press `Enter` to start at **now**, or `Tab` to enter an optional timestamp using the same time formats as the CLI. The quick picker rejects future starts; use **: → Start timer at… (without switching)** for a reviewed start at any CLI-supported timestamp. Its review explicitly labels future starts, and the dashboard labels those timers **Scheduled** until their start time. This reviewed action never stops an existing timer.

In the **quick `s` picker**, if that client already has a running timer, the action explicitly reads **Switch from [scope]**: the old timer stops and the new one starts at the chosen instant in one transaction. If either operation fails, nothing changes. Other clients' timers are untouched.

Success feedback clears after three seconds. Form errors appear beside the relevant field and preserve your input. Stopping or switching reports any time discarded at a monthly cap; these warnings remain until dismissed with `Esc` on the dashboard, even after another successful action. A timer stopped or replaced externally cannot be accidentally stopped or switched using its stale displayed value. Refresh failures disable mutations until the database can be read again.

### Setup and administration

An empty dashboard offers **n → New client**, followed by a project in **2 Projects → n**. You do not need to know CLI syntax or type entity IDs. Client, project, and agreement fields open searchable choices, including client labels and historical agreement coverage dates.

- **New project:** choose a client, enter the name and immutable billing timezone, then enter its initial agreement. Review and create both in one transaction.
- **Pricing:** choose Hourly or Retainer. Hourly terms show a required rate and optional billing minimum. Retainers show a fee, included hours, and optional overage rate; hourly minimums are hidden and never submitted. Both support an independent optional hard monthly maximum. Monetary fields preserve up to six decimal places.
- **Replace agreement:** select a project and review its current terms alongside the new terms and effective date. Existing agreements are never edited in place.
- **Log time:** select a project and agreement, then scope and From/To timestamps. The current agreement is the default; choosing a historical agreement explicitly keeps backfills on their original terms. Reviews show resolved timestamps and duration.
- **Archive/unarchive:** select a project and open `:`. Archival asks for the last active day, defaulting to today in the billing timezone. Unarchival explicitly confirms retroactive continuous coverage. Both explain possible changes to monthly estimates.
- **Settings:** `5`, then `Enter`, changes the display timezone. A successful save immediately updates local input interpretation and dashboard totals, without changing stored UTC timestamps or immutable project billing timezones.

In these forms, `Enter` chooses or advances; it does **not** submit from a text field. Focus the named Continue/Review action, inspect the review, then press `Enter` to confirm. Time values such as `now` are resolved when opening the review, so saving records exactly the reviewed interval. Use `Esc` to edit it again if you want a newer instant. Errors preserve drafts and focus invalid fields. `F1` exposes full help and long error messages at any size. `Esc` goes back; leaving a changed draft asks for **Enter/Esc to keep editing** or **d to discard**. Refresh failures disable saves, not editing. The quick `s` timer picker retains its fast `Enter`-to-start behavior.

### Daily totals

- Today begins at midnight in the active display timezone (loaded at launch or saved in Settings), with daylight-saving transitions respected.
- Completed entries and running timers contribute only their portion between midnight and now. Concurrent and overlapping work is summed, not deduplicated.
- Live work stops earning at its monthly cap, even if the timer remains running.
- Hourly earnings use each interval's original agreement rate. Retainers contribute only the increase in monthly overage during today, accounting for earlier work in that billing month. Fixed fees are never allocated into daily earnings. Monthly minimums and invoice rounding do not apply.
- Different currencies are shown separately, never converted or added together. Display amounts round to two decimal places; this does not change stored work.
- Daily work displays hours and minutes; individual timers retain seconds. Uncapped timers omit allowance text. Colors reinforce text labels rather than replacing them, and selected rows use an explicit high-contrast foreground/background pair.

### Monthly earnings estimate

**This month · estimate** (stacked as **Month (est.)** on small terminals) combines:

- Every retainer's prorated base fee and monthly overage, including retainers with no tracked work yet.
- Completed and capped live hourly work through now, using its historical agreement rates.

Each project contributes its current calendar month in its own billing timezone, matching the retainer breakdown rather than the display timezone. Currencies stay separate. Today's earnings are already included and are never added a second time. Hourly minimums and invoice rounding do not apply. Hourly fractions truncate after summing per currency; retainer charges keep the per-agreement precision described below.

For Vertex, this is **€5,000 + Flyeralarm hours × €125**, with no shapeID overage rate. It is an estimate because fixed fees cover the full agreed period, not just elapsed days.

### Monthly retainer estimates

- Each agreement is shown separately for the current calendar month in its project's billing timezone. Replaced agreements and archived projects remain visible if they covered part of that month.
- Actual hours include completed entries and capped live timers through now, summing overlaps. Unused or excess hours never carry between months or agreements.
- For full-month coverage: `charge = fee + max(0, worked_hours - included_hours) × hourly_rate`. Without an hourly rate, the charge stays at the fee.
- Partial months prorate both the fee and included hours by local calendar days of agreement coverage, just like existing maximums. Replacement dates end the previous agreement's coverage; the archival day is included. Unarchiving restores continuous coverage retroactively.
- The base fee covers the whole agreed period in that month, not just elapsed days. Consequently, replacement or archival can revise the month's estimate and overage threshold. These are live estimates, not issued invoices.
- Prorated included durations truncate to microseconds; base fees and overage charges truncate separately to millionths of currency. The UI rounds money to two decimal places and durations to whole displayed minutes. Invoice rounding is not applied.
- Small terminals show how many retainer rows fit. Press `t` on Dashboard, or choose **Browse all totals and retainers** in `:`, to scroll through every currency and retainer without resizing.

Keyboard actions remain instant. The dashboard has no decorative animation or pulsing indicators.

Completed-entry history browsing, entry editing, mouse support, and configurable keys remain deferred. Historical **agreements** are browsable and usable for logging, but there is currently **no entry-editing workflow**, including in the CLI.

The dashboard targets a personal workspace: it loads project/agreement catalogs and calculates current cap usage synchronously each second. After administration is first opened, the workspace also reloads its client/project/agreement catalogs and rebuilds their searchable rows on refresh. Catalog forms retain their choices and drafts until explicitly changed or reopened. Catalog joins and previews use in-memory scans, not pagination; large catalogs can stall keyboard input. Detail scrolling uses Ratatui's rendered-line counting feature to bound scrolling to the same Unicode wrapping used for display. Daily entry lookup is indexed; one additional indexed range query totals monthly hourly work, clipping to each project's month. The combined estimate reuses existing retainer totals rather than querying each day. Retainer totals read each relevant agreement-month's entries on every refresh (at most two billing months intersect a display day). Very large catalogs or months with many entries can delay input while a refresh runs. The quick project picker uses `nucleo-matcher` synchronously, reusing labels and matcher scratch space and ranking only when the query changes. Large catalogs can still delay typing during a search. Paginated catalogs, background matching (for example, the higher-level `nucleo` crate), and incremental summaries would be needed before scaling this to large shared datasets.

## Time input and storage

The CLI accepts compact local times as a convenience as well as explicit timestamps:

```text
0900
1232
09:00
12:32
2026-03-12 09:00
2026-03-12T09:00:00+01:00
2026-03-12T08:00:00Z
```

Rules:

1. `0900` and `09:00` mean 09:00 today in the configured timezone.
2. A date and time without an offset use the configured timezone.
3. An explicit offset or `Z` overrides the configured timezone.
4. Inputs are converted to UTC before storage.
5. Outputs are converted from UTC to the configured timezone.
6. Overnight entries should use explicit dates rather than relying on an inferred day rollover.
7. Ambiguous or nonexistent local times around daylight-saving transitions are rejected unless an explicit offset is supplied.

The configured timezone is an IANA name such as `Europe/Berlin`, not a fixed offset, so daylight-saving changes are handled correctly.

## Development checks

```console
cargo fmt --check
cargo test --all-targets
cargo test --doc
cargo clippy --all-targets --all-features -- -D warnings
```

TUI layout and picker cursor snapshots live in `src/cli/tui/snapshots/`.
They use a fixed clock and timezone, and run without a real terminal. For intentional
rendering changes, run `cargo test --bin tt cli::tui`, then review the pending
snapshots with `cargo insta review` (install once with `cargo install cargo-insta --locked`).
Commit the accepted `.snap` files, not the pending `.snap.new` files, and rerun the tests.
