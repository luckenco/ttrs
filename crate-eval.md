# CLI parser crate evaluation

We will implement and stabilize the core modules before choosing a CLI argument parser. The parser must remain an adapter over the core rather than shaping its API.

## Candidates

1. `clap` using its derive API
2. `lexopt` using explicit parsing

Both candidates must implement the complete CLI contract in `README.md`. We will pick the faster implementation only after both pass the same correctness and QA suite.

## Experiment structure

Each candidate will be a temporary binary that:

1. Parses arguments into the same plain command types.
2. Calls the same command execution functions.
3. Produces the same output and exit codes.
4. Uses the same core, persistence, time parsing, and interactive code.

Only argument parsing and generated help may differ internally. The losing implementation and its dependencies will be removed after the evaluation.

## Correctness gate

Neither candidate is eligible for benchmarking until all of these pass:

```console
cargo fmt --check
cargo test --all-targets
cargo test --doc
cargo clippy --all-targets --all-features -- -D warnings
```

Public contracts must have Rustdoc examples that compile and run as doctests.

## CLI QA matrix

Every explicit command in `README.md` must have end-to-end tests covering its successful and failing forms:

- `start`
  - Long and short flags
  - Default and explicit start times
  - Multiple concurrent timers
  - Unknown projects and empty scopes
- `stop`
  - Default and explicit stop times
  - Unknown, already stopped, and malformed IDs
  - Stop time before start time
- `status`
  - No running timers
  - One and multiple running timers
- `log`
  - Long and short flags
  - Every supported time format
  - Invalid and reversed intervals
  - Overlapping entries
- `client add` and `client list`
  - Empty and populated client lists
  - Duplicate and invalid client names
- `project add`, `project edit`, and `project list`
  - Projects with and without clients
  - Duplicate and unknown projects
  - Assigning, clearing, and referencing unknown clients
- `config set timezone` and `config get timezone`
  - Valid and invalid IANA timezones
  - Device-timezone fallback
- Bare `tt`
  - Start flow with no running timers
  - Stop selection with running timers
  - Cancellation without mutation
  - Non-TTY behavior
- Parser behavior
  - Missing values and required flags
  - Unknown commands and flags
  - Conflicting options
  - Quoted scopes containing spaces
  - `--help` and `--version`
  - Stable exit codes and diagnostics

Tests should execute the built binaries against isolated temporary data and configuration directories. Parser-specific unit tests may supplement these tests but cannot replace them.

## Performance experiment

Build both candidates with the same locked dependency versions, Rust toolchain, target, and release profile. Benchmark on the same machine while otherwise idle.

Measure with `hyperfine` using warmups and enough runs to expose variance:

```console
hyperfine --warmup 20 --runs 100 '<clap-binary> --help' '<lexopt-binary> --help'
hyperfine --warmup 20 --runs 100 '<clap-binary> status' '<lexopt-binary> status'
```

Also benchmark representative read and write commands against identical prepared databases:

- Empty and populated `status`
- `project list`
- `start`
- `stop`
- `log`
- Invalid invocation and error rendering
- Bare non-interactive invocation

Mutating benchmarks must reset or copy the fixture database before each measured command. Use small everyday data and a large fixture to catch accidental full-table work. Record:

- Median and tail wall-clock time
- Variance across runs
- Release binary size
- Clean and incremental build time
- Dependency count

End-to-end process latency is the primary metric because that is what users experience. Parser-only behavior such as `--help` is included to distinguish argument-parser cost from database work.

## Selection

Correctness and contract parity are mandatory. Once both candidates pass the same QA matrix, select the implementation with lower repeatable end-to-end latency. Document the commands, environment, raw results, and final decision in this file.
