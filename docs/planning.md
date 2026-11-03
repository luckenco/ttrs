# Planned work

Plans are separate reservations, not future completed entries. Adding uses the
same `l` form / `tt log` command. Auto chooses Planned when the start is after the
sampled reference time; otherwise Completed. Explicit type overrides Auto.
Completion always requires reviewed actual times ending at or before now.

## Contracts

- `workspace::Operation::Plan(LogEntry)` validates scope, interval and agreement
  coverage, but neither consumes hard recording capacity nor affects earnings.
  A running timer does not prevent planning.
- `Reschedule` preserves the plan ID and agreement; scope and dates may change.
  `CancelPlan` removes only the reservation.
- `CompletePlan` checks the whole reviewed plan, records actual work under its
  original agreement, and removes the reservation in one transaction. Actual
  work uses existing validation/hard-cap rules. Failure preserves the plan.
- Plans do not auto-complete or expire. Overdue plans continue to reserve hours.
  Work logged independently is not automatically linked: complete the plan
  instead, or explicitly cancel it to avoid double-reserving.
- Plan IDs are distinct from entry/timer IDs and never reused. Checked operations
  reject stale reviews. Remote receipts retain normal exact-replay behavior.
- `ConvertToPlan` explicitly moves a wholly future log into a new plan, preserving
  agreement, scope and exact timestamps. The old entry is removed atomically.
  Existing entries are untouched by migration; they retain their old behavior,
  including hard-cap usage and accrual as their scheduled times pass.
- Replacement/archival must not invalidate plan coverage. Reschedule/cancel
  conflicting plans first. Plans cannot be silently moved to replacement terms.

## Budget semantics

`Query::Budget { month }` requires the first day of the requested calendar month.
Each project uses its immutable billing timezone, regardless of display timezone.
Reservations split by elapsed overlap with billing months and agreement coverage;
DST follows absolute timestamps, not nominal wall-clock duration. Overlaps sum.
Included hours retain existing calendar-day proration, with no carryover between
months or agreements.

Worked remains actual recorded/live work through the snapshot instant. Planned
includes every outstanding reservation overlapping that month, including overdue
ones. Available = included − worked − planned, and may be negative. Neither
reservations nor negative availability can truncate a running timer or produce
actual overage charges. Existing explicit monthly maxima remain independent.

The Dashboard shows current project-local billing months. The budget browser
and `tt budget --month YYYY-MM` also show future months. TUI review and CLI creation
warn about overbooking; warnings are advisory snapshots, not capacity locks.

## Compatibility and safety

Schema 3 adds `tt_plans` without rewriting entries or reusing IDs. Native upgrades
recognize exact supported schemas and verify an independent safety copy before
migration. Worker initialization blocks requests while it publishes and verifies
an R2 safety cut, then compares the cut under the migration transaction. Backup
failure leaves the old schema unchanged. Update the Worker and clients together;
older clients reject schema 3.

Portable format 2 now carries six domain tables/counts and three sequence marks
for schema 3. Schema-1/2 snapshots remain readable and import with empty plans.
Plans, deleted plan-ID high-water marks and plan-operation receipts survive
backup/export/import/restore. Historical schema-2 receipts remain verifiable.

## Limits

This remains a single-user planner, not a calendar/scheduling engine. There is no
recurrence, resource conflict detection, timer linking or automatic completion.
Agreement plan lists and portable snapshots materialize in memory. Review warnings
query each intersected month, so very long multi-year plans can be slow,
especially remotely. Large histories can hit existing response/snapshot limits
and be refused; they are never silently truncated.
