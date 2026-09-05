# Terminal time tracker

> `tt` is the working CLI name. The final project name is still undecided.

A small terminal utility for tracking time against projects and scopes. A scope is either a ticket number or a free-form description; they are intentionally the same concept.

`tt` currently supports macOS and Linux. Native Windows is not supported.

## Is it for you?

`tt` is useful when all of these match how you operate:

- All tracked work is billable under a service agreement.
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
- Agreements define an hourly rate, currency, optional monthly minimum and maximum, and explicit effective date.
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

## Interactive dashboard

Bare `tt` opens a persistent, keyboard-only [Ratatui](https://ratatui.rs/) dashboard. It requires a terminal of at least 50 columns × 16 rows. Explicit CLI commands remain unchanged; bare `tt` without a terminal returns a usage error.

During development, schema changes require recreating the local database. There are no migrations or backward-compatibility guarantees.

The dashboard shows running timers with their project, client, agreement, elapsed time, and monthly allowance. Above them, **worked today** and **earned today** update once per second. Changes made through another CLI process appear automatically.

### Controls

| Key | Action |
| --- | --- |
| `↑` / `↓` or `j` / `k` | Select a running timer |
| `s` | Open the project-and-scope picker to start or switch |
| `x` | Stop the selected timer immediately at now, without confirmation |
| `q` or `Ctrl-C` | Quit; timers keep running |
| `Esc` | Cancel a picker without making changes |

The picker fuzzy-searches active projects by project, client, or agreement name, ignoring case. For example, `wbst` matches `website`. Space-separated words match independently in any order; punctuation such as `!` and `^` is literal, not search syntax. Best matches appear first; ties and an empty query preserve project order. Press `Enter` to choose one, then select one of its three most recent distinct scopes or type a new scope. Recent scopes span replacement agreements. Inside text fields, letters—including `j`, `k`, and `q`—are text; use arrow keys to select suggestions. `Backspace` deletes and `Ctrl-U` clears the field.

Press `Enter` to start at **now**, or `Tab` to enter an optional timestamp using the same time formats as the CLI. Future start times are rejected in the dashboard. If that client already has a running timer, the action explicitly reads **Switch from [scope]**: the old timer stops and the new one starts at the chosen instant in one transaction. If either operation fails, nothing changes. Other clients' timers are untouched.

Stopping or switching reports any time discarded at a monthly cap. A timer stopped or replaced externally cannot be accidentally stopped or switched using its stale displayed value. Refresh failures disable mutations until the database can be read again.

### Daily totals

- Today begins at midnight in the display timezone loaded at launch, with daylight-saving transitions respected.
- Completed entries and running timers contribute only their portion between midnight and now. Concurrent and overlapping work is summed, not deduplicated.
- Live work stops earning at its monthly cap, even if the timer remains running.
- Earnings use each interval's original agreement rate. Monthly minimums and invoice rounding do not apply.
- Different currencies are shown separately, never converted or added together. Display amounts round to two decimal places; this does not change stored work.

Client, project, and agreement administration remains in the CLI. An empty dashboard points to `tt client add` and `tt project add --help`; new projects appear on refresh. History browsing, entry editing, mouse support, and configurable keys are deferred to the future full workspace. There is currently **no entry-editing workflow**, including in the CLI.

The dashboard targets a personal workspace: it loads project/agreement catalogs and calculates current cap usage synchronously each second. Daily entry lookup is indexed, but very large catalogs or months with many entries can delay input while a refresh runs. Project search uses `nucleo-matcher` synchronously, reusing labels and matcher scratch space and ranking only when the query changes. Large catalogs can still delay typing during a search. Paginated catalogs, background matching (for example, the higher-level `nucleo` crate), and incremental summaries would be needed before scaling this to large shared datasets.

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
rendering changes, run `cargo test --bin tt cli::tui::tests`, then review the pending
snapshots with `cargo insta review` (install once with `cargo install cargo-insta --locked`).
Commit the accepted `.snap` files, not the pending `.snap.new` files, and rerun the tests.
