# Isolated TS-host / raw-Wasm feasibility fallback

Synthetic lab only: no production domain rules, billing, workers-rs, wasm-bindgen,
SDK futures, generated JS edits, or credentials beyond the parent's public lab tokens.

`host.ts` exports `Workspace`, `Authority`, and the routing Worker. Each DO owns a
new `WebAssembly.Instance` with SQL imports bound to its storage and that instance's
memory. Rust owns decimal encoding, i128 arithmetic, Jiff MAX and the parent's
1900..2100 America/New_York calendar calculation. These small expressions were
copied from the parent probe: there is no existing pure module to import without
changing parent-owned files. This is not a port of SDK business logic.

## Build and run

From `spikes/remote-host`, using inherited Rust 1.98.1:

```sh
cargo fmt --manifest-path fallback/Cargo.toml --check
cargo test --manifest-path fallback/Cargo.toml --target-dir fallback/target --locked
cargo clippy --manifest-path fallback/Cargo.toml --target-dir fallback/target --locked --all-targets -- -D warnings
cargo clippy --manifest-path fallback/Cargo.toml --target-dir fallback/target --locked --target wasm32-unknown-unknown -- -D warnings
cargo build --manifest-path fallback/Cargo.toml --target-dir fallback/target --locked --release --target wasm32-unknown-unknown
# Parent config selects ./fallback/host.ts in fallback mode.
npx cf build --mode fallback
node fallback/test.mjs
```

The test reads **parent cf build output** and rejects a stale Rust-first bundle.
It uses parent-installed Miniflare 5, `convertV4MiniflareOptions`, real managed
SQLite, persisted runtime restart, and an external service binding that holds
only the sibling's I/O timing. It needs no remote/network credentials. Results
print to stdout; temporary persistence is removed. A three-second timeout is a
failure, never evidence of recovery.

## ABI and transaction boundary

Wasm exports `memory`, `attempt(mode) -> u32`, and `exact()`. The sole import is
`host.sql(id: u32, pointer: u32, length: u32) -> void`. Strings are borrowed UTF-8
and must be consumed before the import returns. No retained pointers, callbacks,
reentrancy, or async Wasm work. The static statement IDs in `host.ts` bind text;
IDs 2 and 4 have no parameters, and ID 5 inserts a real SQL NULL note.

`attempt` writes partial state, revision, and a success receipt before returning
code 1 (domain rejection), invoking failing SQL (mode 2), or trapping (mode 3).
The TS callback throws for code 1 **inside** `transactionSync`; ordinary returned
Err data deliberately commits in the negative control. Only domain rejection
records a receipt, in a separate synchronous receipt-only transaction with no
intervening await; an existing terminal receipt wins. JS exceptions and traps
roll back and discard the instance before a replacement is installed. Neither
records a rejection receipt. Trap recovery means replacing a disposable instance,
not recovering its Rust stack or claiming its destructors ran.

## Evidence and limits

Completed locally:

- Native tests, rustfmt, native and wasm32 clippy with warnings denied, locked
  release wasm32 build.
- Focused suite against real workerd via a temporary esbuild bundle (same host and
  release Wasm, **not** parent cf/Vite output): domain rollback/receipt precedence,
  JS-exception rollback, negative control, exact i64/text/NULL, i128/Jiff values,
  synchronous trap response and replacement reuse, suspended JS sibling resumption
  in both replacement-first and callback-first orderings, namespace isolation,
  persisted state/receipts/exact values and >2^53 SQLite sequence across restart.
- Strict TS check against generated runtime types for compatibility date 2026-10-06.

Parent cf/Vite integration subsequently passed: `cf build --mode fallback` and
`node fallback/test.mjs` load the emitted JS/Wasm with both SQLite classes in
workerd. The main evidence ledger records that run. No remote deployment is required
or permitted for this stage.

**Incomplete:** R2 backup/restore and alarm retry, full cost/crossing/memory ledger,
canonical numeric rejection and full SQLite constraint suite, bounded ingress and
production-grade authentication. The host's token comparison is lab-only; it does
not reproduce the parent's constant-time ingress implementation. An await is a
JS service-binding promise, not a Rust SDK future. This proves the narrow fallback
lifetime gate, not equivalence with the complete Rust-first lab.

Scaling assumption: every live DO gets its own Wasm linear memory and bundled
Jiff tzdb. High live-object counts can increase isolate memory substantially;
per-instance memory, instantiation/startup cost, and cost comparisons remain
unmeasured. SQL statements and receipt keys are synthetic, bounded lab operations,
not an extensible production repository protocol.
