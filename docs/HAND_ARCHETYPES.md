# Hand Archetypes

> Concrete Hand specialisations registered in `src/hand_archetypes.rs`. Each
> archetype maps to a Hand capability class from
> [`docs/HAND_CAPABILITIES.md`](HAND_CAPABILITIES.md) and carries a
> **tool policy** that the spawn path and the CLI enforce mechanically —
> not via prompt suggestion.
>
> Hand archetypes are orthogonal to Head archetypes (see
> [`docs/HEAD_ARCHETYPES.md`](HEAD_ARCHETYPES.md)): Heads decide *which*
> Hand archetype to spawn, Hands execute the work under the archetype's
> contract.

## Registered archetypes

| Archetype         | `HandKind`        | Capability class (`HAND_CAPABILITIES.md`) | Tool policy        | Spawns |
|-------------------|-------------------|-------------------------------------------|--------------------|--------|
| Owner             | `Owner`           | Builder / Surgeon / Refactorer / Scribe / …                 | write-capable      | —      |
| Investigator      | `Investigator`    | Cartographer (read-only)                                     | read-only (chmod)  | —      |
| Release Manager   | `ReleaseManager`  | Merger-adjacent (integration authority, no delegation)       | allow-list         | —      |
| Bug Hunter        | `BugHunter`       | Triager + Surgeon (small defects)                            | write-capable      | —      |
| Performance Engineer | `PerformanceEngineer` | Refactorer + Cartographer (measured perf improvement)  | write-capable      | —      |
| Merger            | `Merger`          | Merger                                                       | write-capable      | —      |
| Head              | `Head`            | n/a (orchestrator; see [`HEAD_ARCHETYPES.md`](HEAD_ARCHETYPES.md)) | write-capable | Hands |

The Hand registry is deliberately narrower than the HAND_CAPABILITIES
taxonomy: several classes (Builder, Refactorer, Scribe, Test Engineer,
Tooler, Migrator, Janitor) share the default **Owner** archetype today
— they are distinguished by spark intent, not by a new registry entry.
Add a new archetype only when it needs its own tool policy OR its own
acceptance bar worth locking in a prompt and a snapshot test (the Bug
Hunter is the motivating case for the second criterion).

## Tool policy, enforced mechanically

Every archetype's tool policy is a compile-time value in
[`src/hand_archetypes.rs`](../src/hand_archetypes.rs). Two layers enforce
it:

1. **Filesystem policy** applied at spawn time by
   `hand_archetypes::apply_tool_policy`. Read-only archetypes get their
   worktree chmod'd `0o444 / 0o555` before the agent subprocess launches,
   so any write syscall the agent attempts fails at the kernel boundary
   with `EACCES`. The only valid escape is `hand_archetypes::unlock_worktree`,
   which the worktree-cleanup path uses before `git worktree remove`.

2. **Command allow-list** evaluated by `hand_archetypes::enforce_action`.
   The CLI resolves the caller's archetype from
   `RYVE_HAND_SESSION_ID → agent_sessions.session_label` and rejects any
   disallowed action (`ryve hand spawn`, `ryve head spawn`, `ryve comment
   add` on a non-release spark, `ryve ember send`) with a non-zero exit
   *before* the action reaches the DB.

The prompt composer for each archetype (e.g.
`agent_prompts::compose_release_manager_prompt`) repeats the contract for
the agent's benefit, but the prompt is not the source of truth. If the
prompt and the tool policy disagree, the tool policy wins.

## Owner

- **`HandKind`:** `Owner`.
- **Capability class:** Builder / Surgeon / Refactorer / Scribe / Test
  Engineer / Migrator / Tooler / Triager / Janitor (any write-capable
  worker role).
- **Tool policy:** write-capable. No command gating beyond the default.
- **Spawn:** `ryve hand spawn <spark_id>` (the default) or `--role owner`.
- **Prompt:** `compose_hand_prompt` in `src/agent_prompts.rs`.
- **Example sparks:** Any single-spark task that touches code in its own
  worktree — the default shape for Crew members.

## Investigator

- **`HandKind`:** `Investigator`.
- **Capability class:** Cartographer (read-only audit).
- **Tool policy:** read-only worktree (chmod'd at spawn; `Edit` / `Write`
  fail at the kernel boundary). Destructive git commands are banished by
  name in the prompt.
- **Spawn:** `ryve hand spawn <spark_id> --role investigator [--crew <id>]`.
- **Prompt:** `compose_investigator_prompt` in `src/agent_prompts.rs`.
  Findings flow **only** as structured comments via `ryve comment add`,
  each citing at least one `file:line`.
- **Typical caller:** Research Head.
- **Example sparks:** "Audit perf_core hot paths for allocations", "Map
  every call site of `assignment_repo::assign` and classify by role".

## Release Manager

Spark ryve-e6713ee7 / [sp-2a82fee7].

The Release Manager is a **singleton** archetype whose entire job is
steering one Release through its lifecycle on Atlas's behalf. Its
communication graph is deliberately narrow: it takes direction only from
Atlas and reports only to Atlas. The enforcement is mechanical — a
`ToolPolicy` allow-list evaluated by the CLI on every mutation, not a
prompt suggestion.

### Atlas-only communication discipline

The Release Manager is the first Ryve archetype with a **narrow comms
graph**. Every other Hand can broadcast (embers), spawn subordinates
(nested `ryve hand spawn`), or cross-comment freely between unrelated
sparks. The RM cannot:

- **Cannot spawn Hands or Heads.** Atlas decides scope and dispatch. A
  RM that spawns subordinates could silently widen a release — the CLI
  rejects `ryve hand spawn` and `ryve head spawn` with
  `archetype 'release_manager': spawning a hand is forbidden by tool
  policy`.
- **Cannot comment on non-release sparks.** Atlas polls the release
  member sparks; that is the only channel back to Atlas. A comment on
  any other spark would bypass Atlas's attention window — the CLI
  rejects it and names the offending spark id.
- **Cannot send embers.** Embers broadcast beyond Atlas and would leak
  release status into the general workgraph signal channel.
- **Cannot edit sparks outside the release it manages.** The RM's
  assignment row pins it to one management spark; filesystem writes
  still go through the git worktree, and any cross-release edit would
  show up in the diff on a branch that does not belong to it. The
  release close flow (`ryve release close`) is the only operation that
  mutates release state in bulk, and it is on the allow-list.

### Tool policy allow-list

Allowed:

- `ryve release *` subcommands (`create`, `list`, `show`, `add-epic`,
  `remove-epic`, `status`, `close`).
- Read-only workgraph queries (`ryve spark list/show`, `ryve bond list`,
  `ryve release list/show`, `ryve crew list/show`, `ryve assign list`,
  `ryve contract list`, `ryve ember list`, `ryve comment list`).
- Git on `release/*` branches only — commit, tag, fetch inside the
  release worktree. No `--force`, no `--no-verify`, no history rewrites.
- `ryve comment add <spark_id>` only when `<spark_id>` is currently a
  member of a release (present in `release_epics`).

Forbidden (rejected at the CLI):

- `ryve hand spawn`, `ryve head spawn`.
- `ryve comment add` on any spark that is not a release member.
- `ryve ember send` of any type.
- Edits to sparks outside the managed release (filesystem boundary —
  every release's work lives on its own branch).

### Spawn

```sh
ryve hand spawn <release_management_spark_id> \
    --role release_manager \
    --agent claude
```

`<release_management_spark_id>` is the spark Atlas creates to track the
release's human-visible progress — its description / problem statement
should name the release id the RM is steering. There is exactly **one**
Release Manager per release; this is a non-goal of the archetype to
parallelise.

### Prompt

Composed by `compose_release_manager_prompt` in
`src/agent_prompts.rs`. The prompt carries the assignment spark's
intent, repeats the allow-list and the Atlas-only comms discipline for
the agent's benefit, and ends with the workflow `ryve release show →
status → close`. A snapshot test
(`release_manager_prompt_locks_identity_and_allow_list_skeleton`) locks
the skeleton so a future edit that softens the contract fails the build.

### Example sparks

- "Manage release 0.1.0" — attached to `rel-…`, Atlas briefs the RM with
  the release id + scope in the spark description.
- "Close release 0.2.0 and record tag + artifact" — close-out RM for a
  nearly-finished release.

## Bug Hunter

Spark ryve-e5688777 / [sp-1471f46a].

The Bug Hunter is a **Triager + Surgeon** hybrid specialised on small
defects. It reproduces the bug with a failing test FIRST, localises the
root cause, and lands the smallest possible diff that flips the test
from red to green. It is one of the highest-leverage specialised Hand
archetypes because the failing-test-first discipline makes every fix
independently verifiable — the same test that caught the bug is the
regression guard going forward.

### Acceptance bar (non-negotiable)

1. **A failing test exists that reproduces the bug.** Written in the
   project's existing test layout and runner — no new harnesses.
2. **The smallest diff that flips the test.** Prefer one-line over
   ten-line, one-file over two-file, root cause over symptom (but never
   rewrite unrelated code). Refactors, cleanups, and adjacent
   improvements are **out of scope** — file a new spark instead.
3. **No existing tests regress.** The agent decides when to run the
   suite (auto-running tests is an explicit non-goal of this archetype;
   see below), but shipping without having checked is not acceptable.

### Language-agnostic by construction

The archetype makes **no assumptions** about project language, test
runner, or framework. The prompt explicitly lists several manifest
files (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`,
`pom.xml`, `build.gradle`, `Gemfile`, `mix.exs`, …) and tells the Bug
Hunter to mirror whichever style the repo already uses. A snapshot
test in `src/agent_prompts.rs`
(`bug_hunter_prompt_locks_identity_and_acceptance_bar_skeleton`)
locks this multi-ecosystem coverage so a future edit cannot narrow
the archetype to one language.

### Non-goal: auto-running tests

The spark intent (ryve-e5688777) explicitly names "auto-running tests
for the agent" as a non-goal. The agent decides when to run what —
`cargo test`, `npm test`, `pytest`, etc. The prompt asks for the tests
to be run before the spark is closed, and the regression test
`bug_hunter_prompt_leaves_test_execution_to_the_agent` guards against
a future edit that slips an auto-run promise into the skeleton.

### Tool policy

- **`HandKind`:** `BugHunter`.
- **Filesystem:** write-capable. Bug Hunters must edit code to land
  both the regression test and the fix, so no read-only chmod is
  applied.
- **CLI allow-list:** unrestricted (same default as `Owner`). Scope is
  policed by the prompt and the DONE checklist, not by
  `hand_archetypes::enforce_action`.

### Spawn

```sh
ryve hand spawn <bug_spark_id> \
    --role bug_hunter \
    --agent claude
```

The `<bug_spark_id>` should be a `type: bug` spark (or similarly
scoped task) whose intent names the symptom and, ideally, a reproducer
shape. Bug Hunters may be dispatched solo (direct spawn) or as part of
a Crew when a Head has decomposed an epic into multiple defects.

### Prompt

Composed by `compose_bug_hunter_prompt` in `src/agent_prompts.rs`. The
prompt carries the bug spark's intent, the acceptance bar (failing
test + smallest diff), a language-agnostic workflow (REPRODUCE →
LOCALISE → FIX → VERIFY → CLOSE), and a HARD RULES block that bans
scope creep (no refactors, no test-harness widening) and destructive
git. A snapshot test locks the skeleton so softening the contract
fails the build.

### Example sparks

- "Bug: panic on empty input to `assignment_repo::assign`" — a single
  concrete defect with a known call site.
- "Bug: `ryve release close` reports success but leaves the release
  row in `open`" — observable misbehaviour with enough signal to
  reproduce.

## Performance Engineer

Spark ryve-1c099466 / [sp-1471f46a].

The Performance Engineer is a **Refactorer + Cartographer** hybrid
specialised on measurable performance improvements. Unlike a Bug Hunter
(whose acceptance bar is a failing-then-passing test) and unlike an
Architect (who never edits code), a Performance Engineer ships a
targeted fix **plus** before/after numbers. Its acceptance bar is a
measured delta vs a baseline, not a test pass. Performance work has
historically been a source of large sparks for Ryve; making it a
first-class archetype prevents Atlas from open-coding the prompt every
time.

### Acceptance bar (non-negotiable)

1. **A baseline measurement exists, captured BEFORE any code changes.**
   Name the hot path, the metric (latency, throughput, allocations,
   bytes transferred, memory residency, etc.), the method, and the raw
   number. A vibes-based "feels slow" is not a baseline.
2. **A post-fix measurement exists, captured the same way.** Same
   workload, same hardware, same toolchain — otherwise the delta is
   meaningless.
3. **The delta meets the spark's acceptance criterion.** If the spark
   names no numeric target, the improvement must be meaningful relative
   to the measurement's noise floor. Inside-noise "wins" are not done.
4. **Before/after numbers are recorded as a comment on the spark.**
   `ryve comment add <spark_id> '<baseline → post-fix (method, workload)>'`.
   Post-mortems diff these comments; a closed perf spark with no recorded
   numbers is treated as unverifiable regardless of the diff it shipped.
5. **No existing tests or benchmarks regress.** The agent decides when
   to run the suite (auto-running is an explicit non-goal); shipping
   without having checked is not acceptable.

### Language-agnostic by construction

The archetype makes **no assumptions** about profiling tools or
benchmark harnesses. The prompt describes WHAT to do (baseline, profile,
propose, verify) and lets the agent pick WHICH tool fits the repo —
`cargo bench` / `criterion` / `perf` / `samply` for Rust; `node --prof` /
`clinic` for Node; `cProfile` / `py-spy` / `scalene` for Python;
`pprof` for Go; `async-profiler` / `JFR` for JVM; `Instruments` /
`dtrace` on macOS; etc. A snapshot test
(`performance_engineer_prompt_locks_identity_and_acceptance_bar_skeleton`)
locks multi-ecosystem coverage so a future edit cannot narrow the
archetype to one toolchain.

### Non-goals (explicit)

- **Shipping a profiler or benchmark harness.** The archetype uses
  whatever the repo already has; if the repo has none, a throwaway
  reproducer is treated as scaffolding, not a deliverable. If the repo
  needs a persistent harness, that is a separate spark.
- **Automated baseline capture.** Baselining is the agent's job this
  turn, not the workshop's job forever. The parent spark's non-goal on
  this point is enforced by a HARD RULE in the prompt.

A regression test
(`performance_engineer_prompt_records_numbers_as_spark_comments`)
guards the comment-as-recording-surface invariant so a future edit
cannot silently relocate the numbers into commit bodies or PR
descriptions where post-mortems can't diff them.

### Tool policy

- **`HandKind`:** `PerformanceEngineer`.
- **Filesystem:** write-capable. Performance Engineers must edit code
  to land the improvement, so no read-only chmod is applied.
- **CLI allow-list:** unrestricted (same default as `Owner`). The
  "measured delta" discipline is enforced by the prompt and the DONE
  checklist, not by `hand_archetypes::enforce_action`.

### Spawn

```sh
ryve hand spawn <perf_spark_id> \
    --role performance_engineer \
    --agent claude
```

The `<perf_spark_id>` should carry a structured intent that names the
hot path and, ideally, a measurable target delta (e.g. "reduce
`render_frame` p99 from 18ms to <8ms"). Performance Engineers may be
dispatched solo or as part of a Crew (e.g. under a Build Head that has
decomposed a perf epic into per-hot-path sparks).

### Prompt

Composed by `compose_performance_engineer_prompt` in
`src/agent_prompts.rs`. The prompt carries the perf spark's intent, the
measurement-based acceptance bar, a four-phase language-agnostic
workflow (BASELINE → PROFILE → PROPOSE → VERIFY → RECORD & CLOSE), and
a HARD RULES block that bans shipping a profiler/harness, adding
automated baseline capture, scope creep (no refactors), changing
observable behaviour, and destructive git. A snapshot test locks the
skeleton so softening the contract fails the build.

### Example sparks

- "Perf: `render_frame` p99 regressed from 8ms to 18ms on large
  workloads" — one hot path, one metric, clear delta to recover.
- "Perf: reduce allocations in `assignment_repo::assign` hot path" —
  named call site + named metric (allocations/call).

## Merger

- **`HandKind`:** `Merger`.
- **Capability class:** Merger.
- **Tool policy:** write-capable. No command gating beyond the default,
  but the prompt enforces **workshop-root isolation**: all merge work
  happens inside `.ryve/worktrees/merge-<crew_id>/`, never in the
  workshop root.
- **Spawn:** `ryve hand spawn <merge_spark_id> --role merger --crew <crew_id>`.
- **Prompt:** `compose_merger_prompt` in `src/agent_prompts.rs`.
- **Typical caller:** Build Head, at the end of its Crew's run.
- **Example sparks:** "Merge crew cr-abcd1234 into one PR".

## Cross-archetype invariants

- **One registry, one spawn seam.** Every archetype is a `HandKind`
  variant wired through `spawn_hand` in `src/hand_spawn.rs`; no
  archetype-specific codepaths live outside `hand_archetypes.rs` and the
  one match arm that routes its prompt composer.
- **Tool policy is mechanical.** If an archetype restricts an action,
  the enforcement is either a filesystem policy (chmod) or a CLI gate
  (`enforce_action` on `RYVE_HAND_SESSION_ID`). Prompt prose is a
  secondary documentation channel, not the enforcement surface.
- **Session label is the archetype identity.** `agent_sessions.session_label`
  is written on spawn and read back by the CLI gate on every invocation.
  It must match the `HandKind::session_label` value one-to-one — the
  regression test `archetype_id_is_stable_per_kind` guards this.
- **Singleton archetypes do not parallelise.** The Release Manager is
  explicitly singleton per release — do not spawn two RMs on the same
  release, do not introduce a second Release Manager archetype.

## Architect

**Capability class:** Reviewer / Cartographer (`docs/HAND_CAPABILITIES.md`).

**Tool policy:** Strictly **read-only**. The Architect MUST NOT use
`Edit`, `Write`, or `NotebookEdit`; MUST NOT run destructive git
(`git reset --hard`, `git push --force`, `--no-verify`, `git branch -D`,
`git checkout -- …`, `git clean -f`); MUST NOT mutate files outside the
`.ryve/` scratch area; MUST NOT install packages or run tests that
change on-disk state. Shell is limited to `ls`, `cat`, `rg`, `grep`,
`find`, read-only `git` commands, and the `ryve` CLI. Attempts to write
are denied at the capability gate at spawn time — the Architect never
produces diffs.

**Mission.** Review the design and architecture of the codebase in the
scope of the parent spark, and produce written recommendations,
tradeoffs, and risks as structured comments on the parent spark. The
Architect answers "how should this be shaped?", not "what does this
look like today?" (that is the Investigator's job).

**Outputs.** Exclusively structured comments on the parent spark, one
recommendation per comment, posted via `ryve comment add <spark_id>
'<body>'`. Each recommendation follows this block:

```
RECOMMENDATION
severity: <blocker|high|medium|low|info>
category: <boundary|coupling|cohesion|layering|data-flow|ownership|observability|evolvability|other>
location: <path/to/file>:<LINE> [, <path/to/other>:<LINE> ...]
recommendation: <the proposed design change, in prose>
tradeoffs: <what the proposal costs — perf, complexity, migration effort>
risks: <what could go wrong; blast radius; rollback story>
alternatives: <other shapes considered, and why this one wins>
```

Every recommendation must cite at least one `file:line`. A
recommendation without evidence is a whitepaper, not an Architect
deliverable, and must not be posted. The final comment is a SUMMARY
listing every recommendation and the Architect's overall read.

**Language neutrality.** The Architect prompt and its recommendation
categories are language- and framework-agnostic by construction.
Categories are generic design concerns (`boundary`, `coupling`,
`cohesion`, `layering`, `data-flow`, `ownership`, `observability`,
`evolvability`) — NOT framework names. A specific technology may appear
inside the `recommendation` body when it is relevant, but never as a
category. The same Architect Hand runs against a Python + TypeScript
monorepo, a Rust crate, or a mixed-stack service without re-tuning.

**Non-goals.**

- The Architect MUST NOT propose, draft, or edit Architecture Decision
  Records (ADRs) autonomously. If a recommendation warrants an ADR, the
  Architect says so in the recommendation body and suggests a follow-up
  spark scoped to a writing Hand — but never creates or modifies files
  under `docs/adr/` or similar paths itself.
- The Architect MUST NOT rewrite the current design in a diff. Output is
  prose-shaped comments; code changes are spawned off as follow-up
  sparks and claimed by a different Hand.

**Distinction from Investigator.** Both archetypes share the same
read-only discipline and comment-based output channel. The Investigator
*maps* existing code (hot paths, unbounded queues, missing logging) and
its deliverable is `FINDING` blocks. The Architect *proposes* how the
code should be shaped going forward; its deliverable is
`RECOMMENDATION` blocks that carry tradeoffs, risks, and considered
alternatives — pieces the Investigator does not produce.

**Spawn shape.**

```sh
ryve hand spawn <review_spark_id> --role architect [--crew <crew_id>] [--agent <a>]
```

The spawn path writes `agent_sessions.session_label = "architect"`,
claims the review spark with `AssignmentRole::Owner`, and tags any crew
membership with role `"architect"`. The initial prompt is emitted by
`compose_architect_prompt` in `src/agent_prompts.rs`.

**Example spark.**

```sh
ryve spark create --type task --priority 2 \
  --problem 'the ingest pipeline and the projection layer share state
             through a module-level singleton; we are hitting races in
             production under concurrent writes' \
  --invariant 'recommendations must be structured comments on the parent
               spark; no source files are modified' \
  --non-goal 'automatically authoring an ADR' \
  --acceptance 'at least one RECOMMENDATION comment with location and
                tradeoffs' \
  --acceptance 'final SUMMARY comment posted before the spark closes' \
  --scope 'src/ingest/, src/projection/' \
  'Architect review: ingest ↔ projection boundary'
```

Then spawn the Architect on it:

```sh
ryve hand spawn <spark_id> --role architect --agent claude
```

The Hand posts one or more `RECOMMENDATION` comments (e.g. *"separate
the write path from the read projection with an explicit queue at
`src/ingest/mod.rs:42`; tradeoff is one extra hop on the hot path;
risk is the call sites in `src/projection/mod.rs:88` may be missed in
the cut-over"*), closes the spark with a SUMMARY, and exits. No files
in the worktree are modified.

**Invariants (must hold).**

1. Architect is strictly read-only; attempts to write are denied at the
   capability gate enforced at spawn time.
2. Outputs are structured comments on the parent spark — never diffs.
3. The prompt is language-neutral; examples reference generic design
   patterns, not language-specific frameworks.

These are locked by the prompt regression test
`architect_prompt_locks_identity_and_read_only_contract` and the
language-neutrality test `architect_prompt_is_language_neutral` in
`src/agent_prompts.rs`, plus the end-to-end integration test
`tests/architect_hand.rs` which runs an Architect against a synthetic
Python + TypeScript project and asserts that no files are mutated.
