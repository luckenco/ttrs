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

## Interactive convenience command

Bare `tt` provides a small interactive dialog when attached to a terminal. Explicit commands remain the stable interface for scripts and other frontends.

### No timers are running

The dialog walks through starting a timer:

```text
$ tt

Project:
1. website (Acme; Support 2026)
2. API (Acme; Retainer 2026)
> 1

Scope:
1. ISSUE-12
2. ISSUE-08
3. Planning
n. New scope
> 2

Start time [now]:
>

Started ISSUE-08 for website at 09:32 (id:1)
```

The scope list contains the three most recently used distinct scopes for the selected project across replacement agreements. Starting a concurrent timer for a different client uses the explicit `tt start` command.

### One or more timers are running

The dialog lists the running timers. Selecting one stops it:

```text
$ tt

Running:
1. ISSUE-02    Support 2026    website    00:42
2. ISSUE-05    Support 2026    API        00:18
> 2

Stopped ISSUE-05 at 12:32 (id:1)
```

Cancelling the dialog makes no changes.

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
