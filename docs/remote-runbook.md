# Connected workspace operation

For personal use: keep the existing safety guards and size limits, do one practical
restore check yourself, and monitor ordinary use. Backups stay in the same Cloudflare
account; no outside-account copy or second operator is required. Account-wide loss
or loss of access remains a shared risk.

No real tracking data has been imported or approved here. The synthetic paid test
used **98 application requests**; its Worker, two DO namespaces/alarms and 62 R2
objects/bucket were removed on 2026-10-17. Protected local evidence was preserved.
Paid subscriptions were not changed. Further benchmarking/profiling is paused.

## Setup

1. Review `worker/cloudflare.config.ts`: resource names, exact HTTPS ORIGIN and
   bindings. The checked-in origin is deliberately non-routable.
2. Supply independent high-entropy GRANTS app tokens and private CONTROL through
   protected secret files. Keep management credentials separate; never pass secret
   values as command arguments or commit them. Grants cover every workspace name.
3. Use the pinned cf CLI for authentication/types/build/deploy. Verify the account
   and preview the exact scope before management changes. Provisioning, deployment,
   deletion and real-data use need explicit approval.
4. Run the [Worker checks](../worker/README.md#local-checks) and root README checks.
   Configure the client, then explicitly bootstrap with an admin credential:

```sh
tt config backend remote --url https://tt.example --workspace personal \
    --credential-env TT_REMOTE_TOKEN
tt workspace create
```

## Normal operation and recovery

- Inspect backup status for freshness, pending cuts and overdue alarms.
  Persistent `changed`/`unverified`, pending failures and storage warnings need
  attention. `verified_unchanged` does not require a newly written hourly file.
- The 50 MB per-DO warning is monitoring only, not automatic cleanup or an alert.
  Control history and R2 objects accumulate; revisit growth if usage approaches limits.
- After an uncertain write, use `tt request list` / `status`; retry only the saved
  request if status permits. Do not create a fresh key or switch identities.
- After uncertain maintenance, inspect `tt restore status [JOB]` and explicitly
  resume the existing job. Never assume timeout means rollback.
- Review the exact whole frozen report before activation. Before flip, abort can
  restore old availability; after flip, resume forward, never reactivate old.

## Practical restore check

Download and verify a trusted synthetic backup, import it into a throwaway native
database, and inspect expected work and running timers through the normal app.
Confirm data/revision/high-water preservation and fresh identity. Do not replace a
live database for this check. See [maintenance commands](data-safety.md#connected-maintenance)
and [invariants and assumptions](worker-contract.md).

Revisit capacity testing only for repeated failures or substantial growth. Peak
isolate memory remains unmeasured and the original interrupted download unexplained;
neither is a claim that a broader production capacity gate passed.
