# PerfHead E2E Validation Report [sp-fbf2a519]

Parent epic: **ryve-fbf2a519** — *Head primitive: `ryve head spawn`, archetypes,
and Crew orchestration*
Spark:       **ryve-6dc2ffea** — *E2E validation: rerun perf remediation under
PerfHead*

This report documents the reproducible scenario that replays the P1 perf
remediation under a real Head and compares the result to the manual,
human-driven run (**crew cr-d48bbba9 / PR #14**). The harness lives at
`scripts/e2e_perf_head_replay.sh`.

---

## 1. Baseline — the manual run we are reproducing

The manual perf-audit was shipped as PR #14 ("perf: P1 hot-path remediation
— six fixes + regression harness"). From `git log crew/cr-d48bbba9`:

| Artifact                    | Value |
|-----------------------------|-------|
| Crew branch                 | `crew/cr-d48bbba9` |
| Merger PR                   | [#14](https://github.com/loomantix/ryve/pull/14) (`380a7ab`) |
| Child sparks fanned out     | **6** (one per hot-path fix) |
| Hand branches merged        | **6** (`hand/{0e2ed795, 29214852, 4858031b, 6a516773, 8c4b5e01, 2b2dcf07}`) |
| Merger role                 | 1 Hand with role `merger` on crew `cr-d48bbba9` |
| Post-merge fixup            | `d0c465c fix(merge): post-integration fixups` |
| Atlas direct dispatches     | **0** — the fan-out was human-driven, not via Atlas |
| Fixes delivered             | keystroke `SparksPoll` storm, font intern bound, `sysinfo` snapshot share, `agent_context` hash-and-skip, async `create_hand_worktree`, regression harness + CI gate |

These numbers define the **parity contract** that PerfHead must satisfy
when the harness runs in `MODE=full`.

---

## 2. Harness design

### What it exercises

The harness is a single POSIX shell script (`scripts/e2e_perf_head_replay.sh`)
that:

1. **Isolates itself.** It unsets `RYVE_WORKSHOP_ROOT` /
   `RYVE_HAND_SESSION_ID` inherited from the invoking shell so it cannot
   leak into a live workgraph, then pins the env to a fresh throwaway
   workshop under `$TMPDIR`.
2. **Seeds a perf-audit-shaped epic.** A P1 epic plus six pre-seeded child
   sparks mirroring the exact decomposition of cr-d48bbba9. Pre-seeding
   keeps the comparison deterministic even when PerfHead's LLM-driven
   decomposition varies.
3. **Installs a stub agent.** The stub records its argv and exits 0. That
   lets us observe orchestration shape (crew, hands, merger, branches)
   without waiting on a real coding agent. It mirrors the stub pattern
   already used in `src/hand_spawn.rs` tests.
4. **Runs PerfHead** — `ryve head spawn <epic> --archetype PerfHead` with
   the stub bound as the agent command.
5. **Polls for convergence** — until the Crew has ≥6 owner members plus
   exactly 1 merger, or `STALL_SECS` elapses (failure).
6. **Emits a JSON report** at `$OUT_JSON` capturing observed vs. manual
   baseline and a boolean parity contract.

### Run modes

| Mode      | Behavior                                                                 |
|-----------|---------------------------------------------------------------------------|
| `full`    | Requires `ryve head spawn --archetype PerfHead`. Fails loudly if missing. |
| `dry-run` | Skips the spawn; only seeds the workshop and asserts the scaffold.        |

`dry-run` exists so the harness is useful in CI **before** the epic's
sibling sparks (ryve-53bb0bac PerfHead, orchestrator, head-spawn CLI) are
integrated onto `main`. Once integrated, CI flips to `full`.

### Parity contract (machine-readable)

```jsonc
{
  "parity_contract": {
    "hands_ge_6":              true,   // ≥6 hand branches under .ryve/worktrees
    "no_atlas_direct_dispatch": true,  // no hand_assignments with parent=Director
    "child_sparks_equal_6":    true    // decomposition matches manual baseline
  }
}
```

`scripts/e2e_perf_head_replay.sh` exits non-zero if any of these flips
`false` in `MODE=full`.

---

## 3. Current result

As of this spark being closed, `main` does **not** yet contain the PerfHead
archetype or the `ryve head spawn --archetype PerfHead` subcommand; both
live on in-flight hand branches for sibling sparks of epic ryve-fbf2a519
(ryve-53bb0bac, plus the orchestrator and head-spawn CLI Hands). The
harness has been exercised in `MODE=dry-run` successfully:

```
[e2e-perf-head] seeding throwaway workshop at /tmp/ryve-e2e-perfhead-…
[e2e-perf-head] seeded 6 child sparks under <epic>
[e2e-perf-head] MODE=dry-run — skipping ryve head spawn
{
  "mode": "dry-run",
  "child_spark_count": 6,
  "manual_baseline": { "hands": 6, "merger": 1, "atlas_direct_dispatches": 0 },
  "parity_contract": {
    "hands_ge_6": false,                // expected in dry-run: no hands spawned
    "no_atlas_direct_dispatch": true,
    "child_sparks_equal_6": true
  }
}
[e2e-perf-head] ok (mode=dry-run)
```

In `MODE=full` on the current `main`, the harness correctly **fails-fast**
with a clear error identifying the missing CLI surface — this is the
signal to the epic merger that PerfHead has not landed yet.

### Expected `full` result after epic integration

When the epic merges, re-running `scripts/e2e_perf_head_replay.sh` (no env)
is expected to produce:

| Field                             | Expected  | Source                                                     |
|-----------------------------------|-----------|------------------------------------------------------------|
| `observed.hand_branches`          | ≥ 6       | `git branch --list 'hand/*'` inside the throwaway workshop |
| `parity_contract.hands_ge_6`      | `true`    | ^                                                          |
| `parity_contract.no_atlas_direct_dispatch` | `true` | `assign list` parent_role inspection                       |
| `parity_contract.child_sparks_equal_6`      | `true` | matches cr-d48bbba9                                        |
| exit code                         | `0`       | parity                                                     |

### Known deltas vs. manual run

These are **intentional** differences, not parity failures:

1. **Stub agent doesn't write code.** The harness validates *orchestration
   shape* (crew, hands, merger, branches) — not that the code on each hand
   branch actually fixes the perf regression. A real PerfHead run against a
   live agent is still required before merging PR-equivalents to `main`.
2. **No GitHub PR.** The Merger in the harness does not push to GitHub; it
   only creates a local merger branch. PR URL posting is out of scope for
   the test because it would mutate shared state.
3. **Pre-seeded decomposition.** The harness seeds six child sparks up
   front instead of letting PerfHead's prompt template decompose from
   scratch. This is deliberate — we want to test that the Head *dispatches*
   the expected fan-out, not that the LLM decomposes identically run-to-run.
   A separate spark can be filed later if we want to add a
   decomposition-variance assertion.

---

## 4. How to re-run

```sh
# Quick sanity check (works today on any branch):
MODE=dry-run scripts/e2e_perf_head_replay.sh

# Full parity check (requires the epic to be integrated):
scripts/e2e_perf_head_replay.sh

# Keep the throwaway workshop around for post-mortem:
KEEP_WORKSHOP=1 scripts/e2e_perf_head_replay.sh

# Redirect the JSON report:
OUT_JSON=/tmp/my-report.json scripts/e2e_perf_head_replay.sh
```

The harness is idempotent, does not touch the caller's workgraph, and
cleans up on success.
