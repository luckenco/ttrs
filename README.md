# Terminal time tracker

> `tt` is the working CLI name. The final project name is still undecided.

A small terminal utility for tracking time against projects and scopes. A scope is either a ticket number or a free-form description; they are intentionally the same concept.

`tt` currently supports macOS and Linux. Native Windows is not supported.

## Is it for you?

`tt` is useful when all of these match how you operate:

- All tracked work is billable under a service agreement.
- Agreement terms do not change; a rate or limit change creates a new agreement.
- Each project uses one current agreement, while historical work keeps its original agreement.
- Work may belong directly to an agreement without belonging to a project.
- Monthly minimums and maximums follow calendar months in the agreement's IANA timezone.
- Concurrent timers may overlap, and that overlap must remain unadjusted.
- You work on macOS or Linux; native Windows is not a target.

## Concepts

- Every time entry has a service agreement and a scope.
- A time entry may also reference a project.
- Clients and immutable service agreements are created explicitly.
- Agreements define an hourly rate, currency, optional monthly minimum and maximum, and billing timezone.
- A project selects one active agreement at a time.
- More than one timer may run at once.
- Overlapping timers remain overlapping; their time is not divided or adjusted.
- All timestamps are stored in UTC and displayed in the configured timezone.

## CLI contract

### Start a timer

```console
tt start --project <project> --scope <scope> [--at <time>]
tt start -p <project> -s <scope> [--at <time>]
tt start --client <client> --agreement <agreement> --scope <scope> [--at <time>]
```

`--at` defaults to now. A project uses its currently selected agreement. Agreement-level time may be recorded without a project by supplying `--client` and `--agreement`.

Examples:

```console
tt start --project website --scope ISSUE-123
tt start -p internal -s "Planning meeting" --at 0900
```

Starting a timer does not stop any other running timer.

### Stop a timer

```console
tt stop --id <timer-id> [--at <time>]
```

`--at` defaults to now. Timer IDs are shown by `tt status` and returned by `tt start`.

### Show running timers

```console
tt status
```

Example output:

```text
ID   AGREEMENT     PROJECT    SCOPE              STARTED   ELAPSED
1    Support 2026  website    ISSUE-02           09:00     01:32
2    Support 2026  -          Planning meeting   10:15     00:17
```

### Log a completed entry

```console
tt log --project <project> --scope <scope> --from <time> --to <time>
tt log -p <project> -s <scope> -f <time> -e <time>
tt log --client <client> --agreement <agreement> --scope <scope> --from <time> --to <time>
```

Example:

```console
tt log -p website -s ISSUE-123 -f 0900 -e 1232
```

The end must be later than the start. Completed entries may overlap running timers or other completed entries.

### Manage clients, agreements, and projects

```console
tt client add --name <name>
tt client list

tt agreement add --client <client> --name <name> --rate <amount> --currency <code> \
  [--minimum-hours <hours>] [--maximum-hours <hours>] --timezone <IANA-timezone>
tt agreement list [--client <client>]
tt agreement archive --client <client> --name <name>

tt project add --name <name> --client <client> --agreement <agreement>
tt project edit --name <name> --agreement <agreement>
tt project list
```

Agreement terms are immutable. Create a new agreement when a rate or monthly limit changes, then switch the project to it. Existing timers and entries retain the agreement selected when they were created. A project and its agreement must belong to the same client.

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
1. website (Acme)
2. API (Acme)
> 1

Scope:
1. ISSUE-12
2. ISSUE-08
3. Planning
n. New scope
> 2

Start time [now]:
>

Started ISSUE-08 for website at 09:32
```

The scope list contains the three most recently used distinct scopes for the selected project. Starting a second concurrent timer is done with the explicit `tt start` command.

### One or more timers are running

The dialog lists the running timers. Selecting one stops it:

```text
$ tt

Running:
1. ISSUE-02    Support 2026    website    00:42
2. ISSUE-05    Support 2026    API        00:18
> 2

Stopped ISSUE-05 at 12:32
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
