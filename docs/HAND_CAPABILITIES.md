# Hand Capability Classes

> Taxonomy of the work that **Hands** (worker coding agents) do inside a Ryve
> workshop. Atlas (the Director) and Heads (Crew orchestrators) consult this
> taxonomy when deciding which kind of Hand to spawn for a given spark.

## Why a taxonomy?

A Hand is a coding-agent subprocess (claude / codex / aider / opencode) launched
into a worktree on a single spark. Although every Hand is mechanically the same
process, the *kind of work* varies enormously: a 5-line bug fix has nothing in
common with a multi-file refactor or a long-running test triage. Without a
shared vocabulary:

- Atlas cannot route a spark to the right Head (and the right Head prompt).
- A Head cannot pick a sensible coding agent, model size, or temperature.
- Reviewers cannot tell at a glance whether a closed spark exercised the part
  of the system the title implies.
- Post-mortems cannot compare success rates across "the same kind of work."

The classes below are **roles a Hand plays for the duration of one spark**, not
permanent labels on agents. The same `claude` subprocess can be spawned as a
`Surgeon` today and a `Cartographer` tomorrow — what changes is the system
prompt, the tooling expectations, and the acceptance bar.

## The Classes

| Class           | One-line role                                           | Typical spark types          | Risk  |
|-----------------|---------------------------------------------------------|------------------------------|-------|
| Surgeon         | Targeted fix in a small, well-localized area of code    | bug, hotfix                  | low   |
| Builder         | Implement a new feature end-to-end against a clear spec | task, feature                | normal|
| Refactorer      | Restructure existing code without changing behavior     | refactor, tech-debt          | high  |
| Cartographer    | Read-only investigation; produce a written map or report| research, spike, audit       | low   |
| Scribe          | Author or update documentation, prompts, examples       | docs                         | low   |
| Test Engineer   | Add tests, raise coverage, or stabilize flaky suites    | test, qa                     | normal|
| Triager         | Reproduce, classify, and route incoming bugs/issues     | triage, intake               | low   |
| Merger          | Integrate a Crew's worktrees into one PR (Crew role)    | merge                        | high  |
| Migrator        | Move data, schema, or APIs from version A to B          | migration                    | high  |
| Tooler          | Build/maintain dev infra: scripts, CI, lint, hooks      | infra, devx                  | normal|
| Reviewer        | Read-only critique of a diff or design                  | review                       | low   |
| Janitor         | Mechanical cleanup: dead code, lint, formatting         | chore                        | low   |

Twelve classes. Fewer is better than more — if a spark genuinely doesn't fit,
that is a signal to either split the spark or extend the taxonomy with intent.

## Class details

Each class lists: **mission**, **inputs the Hand needs**, **outputs it must
produce**, and **example sparks** drawn from the kind of work this codebase
already tracks.

### 1. Surgeon

- **Mission:** Fix a single, well-understood defect in a small radius. The
  acceptance criterion is usually a failing test that should pass.
- **Inputs:** Reproduction steps, expected vs actual behavior, suspected file
  or function. Often a stack trace.
- **Outputs:** Smallest possible diff. New regression test. Commit message
  references the spark.
- **Acceptance bar:** Bug no longer reproduces; no behavior change outside
  the defect's blast radius.
- **Example sparks:**
  - "Validate token expiry on `/me` after logout" (`[sp-a1b2]`)
  - "Spark picker crashes when title contains a tab character"
  - "`ryve hand list --json` emits trailing comma on empty result"

### 2. Builder

- **Mission:** Implement a new feature against a written spec. Bigger blast
  radius than a Surgeon, but the design is already settled.
- **Inputs:** Spec or plan doc (e.g. `docs/HEAD_PLAN.md`), acceptance criteria,
  list of files to touch.
- **Outputs:** Working feature, tests at unit + integration level, doc updates
  if user-visible.
- **Acceptance bar:** Acceptance criteria from spark intent all pass; no new
  warnings; existing tests still pass.
- **Example sparks:**
  - "Add `ryve hand spawn` CLI subcommand"
  - "Bench dropdown gains `New Head` / `New Hand` items"
  - "Implement `crew_repo` with create/list/add-member/status"

### 3. Refactorer

- **Mission:** Restructure code without changing observable behavior. The
  *test suite* is the contract; if it still passes, the refactor is correct.
- **Inputs:** Target file/module, the smell being removed, and the shape of
  the desired end state.
- **Outputs:** Reorganized code, unchanged public API (or a migration plan if
  the API must change), green test suite.
- **Acceptance bar:** No test changes other than imports/paths; no behavior
  changes; reviewer can articulate "what got better."
- **Example sparks:**
  - "Extract `compose_hand_prompt` from `main.rs` into `agent_prompts.rs`"
  - "Split monolithic `cli.rs` into a `cli/` module folder"
  - "Replace ad-hoc spark-id parsing with a `SparkId` newtype"

### 4. Cartographer

- **Mission:** Read-only investigation. Map an unfamiliar area, answer a
  question, or produce a written report that *future* sparks can build on.
- **Inputs:** A question or area of uncertainty.
- **Outputs:** A markdown document (often under `docs/`) or a comment thread
  on the spark. **No code changes.**
- **Acceptance bar:** The next Hand to touch this area can act on the report
  without re-doing the investigation.
- **Example sparks:**
  - "Document how `agent_sessions` rows are reaped on workshop close"
  - "Map every call site of `assignment_repo::assign` and classify by role"
  - "Audit places where `.ryve/sparks.db` is opened directly"

### 5. Scribe

- **Mission:** Author or update human-readable artifacts: docs, READMEs, agent
  prompts, examples, changelogs.
- **Inputs:** Source of truth (code, schema, behavior) plus a target audience.
- **Outputs:** Markdown/prose changes. Sometimes prompt text. Rarely code.
- **Acceptance bar:** Doc accurately reflects current code; new contributor
  can follow it without help.
- **Example sparks:**
  - "Define Hand capability classes" *(this very spark)*
  - "Write `compose_merger_prompt` system prompt copy"
  - "Update `WORKSHOP.md` with the `ryve hand` subcommands"

### 6. Test Engineer

- **Mission:** Improve the safety net. Add missing tests, raise coverage on
  a fragile module, or stabilize a flaky suite.
- **Inputs:** Module/feature to cover, current coverage gap, or a flaky test
  log.
- **Outputs:** New or fixed tests. Occasionally a tiny production-code change
  to make code testable (extract a seam, inject a clock, etc.).
- **Acceptance bar:** Tests pass deterministically across N consecutive runs;
  the previously uncovered branch is now exercised.
- **Example sparks:**
  - "Add round-trip test for `AssignmentRole::Merger`"
  - "Stabilize `cli_hand_spawn` integration test (currently flakes on CI)"
  - "Cover `crew_repo::attach_sparks` error paths"

### 7. Triager

- **Mission:** Take an unstructured incoming report, reproduce it, decide what
  it is, and route it. Often the *output* is a new well-formed spark, not a
  code change.
- **Inputs:** A bug report, user message, ember, or stack trace.
- **Outputs:** Reproduction steps, classification (bug/feature/duplicate/
  not-a-bug), priority, and either a new spark or a comment closing the
  intake spark.
- **Acceptance bar:** Anyone reading the resulting spark knows exactly what
  to do next.
- **Example sparks:**
  - "Triage: 'Ryve hangs on workshop open after upgrade'"
  - "Classify the 6 unowned bug reports filed this week"
  - "Reproduce flaky `agent_sessions` race seen in `flare` ember"

### 8. Merger

- **Mission:** Integrate the worktrees of a finished Crew into a single
  reviewable PR. This is a Crew-only role; see `docs/HEAD_PLAN.md` for the
  full prompt.
- **Inputs:** A Crew id whose member sparks are all `completed`.
- **Outputs:** An integration branch (`crew/<id>`), a single PR linking every
  member spark, a comment on the merge spark with the PR URL.
- **Isolation invariant:** The Merger NEVER changes the branch checked out
  in the workshop root — the user works there, and moving its HEAD is
  disruptive. All merge work happens inside a dedicated worktree at
  `.ryve/worktrees/merge-<crew_id>/` that the Merger creates via
  `git worktree add -b crew/<id> origin/main` and removes via
  `git worktree remove` on exit. `git checkout`, `git switch`, `git pull`,
  and `git reset --hard` are forbidden in the workshop root.
- **Acceptance bar:** PR is open, builds, and lists every member spark.
  The Merger should rarely hit conflicts: the Head that spawned the Crew
  is required to apply merge-clean bond discipline
  (`docs/HEAD_ARCHETYPES.md#cross-archetype-invariants`), serialising any
  siblings whose file scopes overlap via `blocks` bonds. An unresolvable
  conflict is therefore a bond-discipline failure by the Head — the
  Merger posts `bond-discipline conflict in <file> between hand/<a> and
  hand/<b>: …` and sets `spark status blocked` rather than attempting a
  manual resolution.
- **Example sparks:**
  - "Merge crew `cr-136bd4e7` into a single PR"
  - "Open integration PR for the auth-rewrite Crew"

### 9. Migrator

- **Mission:** Move data, schema, or callers from version A to version B.
  Spans schema migrations, API renames, dependency upgrades, and config-file
  format changes. **Never edit applied sqlx migrations** — add a new file.
- **Inputs:** Source format, target format, list of producers/consumers.
- **Outputs:** Forward migration, backfill if needed, updated callers, and a
  rollback story (or an explicit "no rollback" note on the spark).
- **Acceptance bar:** Old code is gone or feature-flagged off; new code is
  the only path; existing data round-trips.
- **Example sparks:**
  - "Migrate `crews` table: add `status`, `head_session_id`, `parent_spark_id`"
  - "Rename `AssignmentRole::Assistant` to `Helper` across the workspace"
  - "Bump `iced` 0.14 → 0.15 and update vendored `iced_term`"

### 10. Tooler

- **Mission:** Improve developer experience. Build or fix scripts, CI jobs,
  pre-commit hooks, lint configs, or local dev tooling.
- **Inputs:** A point of friction in the current workflow.
- **Outputs:** Working script/job/config plus docs on how to use it.
- **Acceptance bar:** Tool works for the next contributor without hand-holding.
- **Example sparks:**
  - "Add `cargo deny` check to CI"
  - "Pre-commit hook: reject commits without `[sp-xxxx]` tag"
  - "`make smoke` runs the full CLI integration suite locally"

### 11. Reviewer

- **Mission:** Read-only critique of a diff, plan, or design. Output is
  comments, not code.
- **Inputs:** A PR, a planning doc (e.g. `docs/HEAD_PLAN.md`), or a
  draft architecture.
- **Outputs:** Structured feedback: blocking issues vs nits, with line refs.
- **Acceptance bar:** Author can act on every blocking comment without
  needing to ask follow-up questions.
- **Example sparks:**
  - "Review PR daxxsyntax/ryve#10 for workgraph invariant violations"
  - "Critique `HEAD_PLAN.md` against the Atlas → Heads → Hands hierarchy"
  - "Architectural review: does `crew_repo` respect bond invariants?"

### 12. Janitor

- **Mission:** Mechanical cleanup. Dead code, unused imports, formatting,
  lint warnings, doc typos. The spark is "boring on purpose."
- **Inputs:** A list of warnings, a `cargo +nightly udeps` report, or a
  reviewer's nit list.
- **Outputs:** A diff that touches many files but changes no behavior.
- **Acceptance bar:** Lint/format/warning count strictly decreases; no
  behavior change.
- **Example sparks:**
  - "Remove all `#[allow(dead_code)]` from `data/`"
  - "`cargo fmt` the workspace; nothing else"
  - "Fix typos in `docs/ARCHITECTURE.md`"

## Choosing a class

When Atlas or a Head reads a new spark, it should ask, in order:

1. **Is the work read-only?** → `Cartographer`, `Reviewer`, or `Triager`.
2. **Is the spark "do not change behavior"?** → `Refactorer` or `Janitor`.
3. **Is the spark "fix one bug"?** → `Surgeon`.
4. **Is the spark "ship a new thing per a spec"?** → `Builder`.
5. **Does the work cross a version boundary (data, API, dep)?** → `Migrator`.
6. **Is the work about the safety net or dev loop?** → `Test Engineer`
   or `Tooler`.
7. **Is the work prose, not code?** → `Scribe`.
8. **Is the work integrating a finished Crew?** → `Merger`.

If two classes both fit, pick the one with the **higher risk tier** — it sets
a stricter acceptance bar, which is the safer default.

## Relationship to existing concepts

- **`AssignmentRole`** (`data/src/sparks/types.rs`): `Owner`, `Assistant`,
  `Observer`, `Merger`. Capability *classes* are orthogonal to assignment
  *roles*. A `Surgeon` is almost always an `Owner`; a `Reviewer` is almost
  always an `Observer`; a `Merger` is the one place where the role and the
  class share a name on purpose.
- **Crews**: a Crew typically contains several `Builder`s + one `Merger`,
  but mixed Crews are allowed (e.g. a `Cartographer` to map the area first,
  then `Builder`s, then a `Merger`).
- **Spark `type` field**: today this carries values like `task`, `bug`,
  `feature`, `epic`. Capability classes are a finer-grained complement and
  can be inferred from the type plus the spark's intent. Future work
  (out of scope here) may add an explicit `capability_class` column on
  `sparks` so Atlas's routing decisions are auditable.

## Non-goals

- This document does **not** prescribe which coding agent (claude / codex /
  aider / opencode) maps to which class. That's a runtime decision Heads
  make based on availability, cost, and the spark's risk tier.
- This document does **not** introduce a new database column or CLI flag.
  It's a shared vocabulary for Atlas, Heads, and human reviewers. Schema
  follow-up, if any, belongs in a separate spark.
