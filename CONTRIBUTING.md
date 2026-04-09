# Contributing to Ryve

Thanks for hacking on Ryve. The day-to-day workflow lives in `.ryve/WORKSHOP.md`
(read it before claiming a spark). This file covers the things that don't change
spark-to-spark — code layout, the test suite, and the performance regression
harness.

## Workspace layout

```
ryve/                     # binary crate (the GUI)
  src/                    # iced application code
data/                     # persistence, config, sparks DB, git helpers
ipc/                      # Hand <-> Workshop IPC
llm/                      # LLM client adapters
perf_core/                # pure, benchmarkable hot-path helpers (see below)
perf-budgets.toml         # performance budgets the CI gate enforces
scripts/perf-gate.py      # reads criterion output, fails CI on regressions
```

## Tests

```sh
cargo test --workspace
```

The test suite is the source of truth for "is this code correct". Most modules
have inline `#[cfg(test)]` units; integration tests live next to their crate
under `tests/`.

## Performance regression harness

Spark `ryve-5b9c5d93` introduced a regression harness so future changes can't
silently re-introduce the slowdowns the perf audit (`ryve-27a217db`) caught.
The harness has three pieces:

1. **Criterion benchmarks** for the hottest pure functions on the UI loop.
2. **A baseline budget file** (`perf-budgets.toml`) checked into the repo.
3. **A CI gate** (`.github/workflows/perf.yml`) that fails the PR when any
   benchmark exceeds its budget by more than the configured tolerance.

A separate **SparksPoll dispatch smoke test**
(`perf_core/tests/sparks_poll_smoke.rs`) runs on every PR. It drives a
synthetic key-event burst through the *exact* dispatch routine the binary
uses (`perf_core::classify_key_event`) and asserts the SparksPoll dispatch
count stays at zero. This catches the antipattern where unmatched key events
get routed to `Message::SparksPoll` (a workgraph reload), which made every
keystroke trigger N agent-session queries.

### Where the benchmarked functions live

All functions the harness measures live in **`perf_core/`**, and the binary
calls into them so the benchmarks track real production code:

| Bench                              | Source                                                  | Why it's hot                                  |
| ---------------------------------- | ------------------------------------------------------- | --------------------------------------------- |
| `process_is_alive`                 | `perf_core::process_is_alive`                           | Polled per Hand on every workshop refresh     |
| `session_filter`                   | `perf_core::count_active_sessions`                      | Hands panel re-render                         |
| `file_git_status_dir_aggregation`  | `perf_core::file_git_status`                            | File explorer redraw, per visible directory   |
| `classify_key_event_unmatched`     | `perf_core::classify_key_event`                         | Every keystroke                               |
| `agent_context_sync_noop`          | `data::agent_context::sync` (steady-state)              | Workshop tick                                 |

### Running the benchmarks locally

```sh
# Fast feedback loop — only the perf_core benches:
cargo bench -p perf_core --bench perf

# Includes the data crate's tokio-driven benchmark:
cargo bench -p data --bench agent_context_sync

# Then check budgets:
python3 scripts/perf-gate.py
```

Criterion writes results into `target/criterion/<bench_name>/new/estimates.json`.
The gate script reads `mean.point_estimate` (nanoseconds) from each file and
compares it against `budget_ns * (1 + tolerance_pct / 100)` from
`perf-budgets.toml`.

The gate exits:

- `0` — every benchmark within budget.
- `1` — at least one benchmark over budget. **CI fails.**
- `2` — budgets declared but no measurement found (you forgot `cargo bench`).

### Adding a new perf gate

1. **Pick a function.** It should be pure (no I/O, no globals), live on a
   path the GUI hits at high frequency, and be cheap to call from a benchmark
   without elaborate fixtures. If the function does I/O — like
   `data::agent_context::sync` — add the bench under that crate and use
   `criterion`'s `to_async(&rt)` adapter.
2. **Move the function into `perf_core/`** (or its existing crate) and have
   the binary call into it. The whole point of the harness is that it
   measures the *same* code the user runs; copies will drift.
3. **Add a benchmark.** For pure CPU functions, drop a `bench_*` function
   into `perf_core/benches/perf.rs`. For async/I/O functions, create a new
   `*.rs` file under `data/benches/` (or whichever crate owns it) and a
   matching `[[bench]]` entry in that crate's `Cargo.toml`.
4. **Run the bench locally** and read the reported mean from the criterion
   table.
5. **Add an entry to `perf-budgets.toml`** with `budget_ns` set to that
   mean. Pick a `tolerance_pct` generous enough to absorb CI noise — start
   at `50` for CPU benches, `100` for anything that touches the filesystem
   or tokio, and tighten over time.
6. **Wire the bench into CI.** If you added a new `[[bench]]` to a crate
   not currently invoked from `.github/workflows/perf.yml`, add a
   `cargo bench -p <crate> --bench <name>` step before the `Enforce
   performance budgets` step.
7. **Reference the spark** in the commit message. Performance changes are
   load-bearing on the audit backlog (`ryve-27a217db`), so a `[sp-xxxx]`
   tag makes the change easy to trace.

### When a budget needs to change

A perf improvement landed? Drop the budget. A perf regression landed because
a feature genuinely needs more work to do? **Don't quietly raise the budget
to make CI green.** Open a separate PR that:

- bumps the budget,
- explains *why* in the commit message (link to the audit comment or spark
  that justified it),
- and ideally cites a follow-up spark that will reclaim the budget.

The harness exists to make perf a deliberate, visible decision — not a
silent backslide.
