# Terminal time tracker

> `tt` is the working CLI name. The final project name is still undecided.

A small terminal utility for tracking time against projects and scopes. A scope is either a ticket number or a free-form description; they are intentionally the same concept.

## Concepts

- Every time entry has a project and a scope.
- Clients are created explicitly.
- A project may reference one client.
- More than one timer may run at once.
- Overlapping timers remain overlapping; their time is not divided or adjusted.
- All timestamps are stored in UTC and displayed in the configured timezone.

## CLI contract

### Start a timer

```console
tt start --project <project> --scope <scope> [--at <time>]
tt start -p <project> -s <scope> [--at <time>]
```

`--at` defaults to now. The project must already exist.

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
ID       PROJECT    SCOPE              STARTED   ELAPSED
a81f2c   website    ISSUE-02           09:00     01:32
c102b7   internal   Planning meeting   10:15     00:17
```

### Log a completed entry

```console
tt log --project <project> --scope <scope> --from <time> --to <time>
tt log -p <project> -s <scope> -f <time> -e <time>
```

Example:

```console
tt log -p website -s ISSUE-123 -f 0900 -e 1232
```

The end must be later than the start. Completed entries may overlap running timers or other completed entries.

### Manage clients and projects

```console
tt client add --name <name>
tt client list

tt project add --name <name> [--client <client>]
tt project edit --name <name> [--client <client> | --clear-client]
tt project list
```

`--client` selects an existing client; an unknown client is an error. Clients and projects are created explicitly so a typo cannot silently create one.

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
2. internal
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
1. ISSUE-02    website    00:42
2. ISSUE-05    internal   00:18
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
