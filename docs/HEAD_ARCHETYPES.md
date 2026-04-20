# Head Archetypes [sp-cc5f4369]

This document catalogs the **standard Head archetypes** used by Ryve's
agent hierarchy: **Atlas (Director) → Heads → Hands**.

A **Head** is a launched coding-agent subprocess (mechanically the same
as a Hand) that orchestrates a Crew of Hands working in parallel. Atlas,
the user-facing Director, decides which archetype to spawn for a given
goal; the archetype determines the Head's system prompt, the kinds of
sparks it is allowed to create, and the kinds of Hands it may delegate
to.

Archetypes are a *prompting and delegation contract*, not a new
subprocess type. Every Head still spawns through `ryve hand spawn`
(see `docs/HEAD_PLAN.md`) and is bound by the same workgraph rules.

---

## Standard archetypes

| Archetype | One-line purpose | Default crew shape | Closes spark by |
|-----------|-----------------|--------------------|-----------------|
| **Build** | Ship code that satisfies acceptance criteria | 2–8 implementer Hands + 1 Merger | Open PR via Merger |
| **Research** | Reduce uncertainty before code is written | 1–4 investigator Hands, no Merger | Posting findings + recommendation comment |
| **Review** | Critique existing code, designs, or PRs | 1–3 reviewer Hands, no Merger | Posting structured review comment(s) |

The three archetypes below are the *standard* set. Atlas may compose
them (e.g. a Research Head whose findings spawn a Build Head) but it
must not invent new archetypes ad-hoc — new archetypes require a spark
and an entry in this file.

---

## Build Head

**Purpose.** Take an epic or feature spark with concrete acceptance
criteria and drive it to a single reviewable PR.

**Inputs.**
- A parent spark with a populated `acceptance_criteria` intent.
- Optional invariants and non-goals.

**Responsibilities.**
1. Decompose the parent spark into 2–8 child task sparks
   (`ryve spark create --type task --acceptance "..."`).
2. Create a Crew (`ryve crew create … --parent <epic>`).
3. For each child spark, spawn an implementer Hand
   (`ryve hand spawn <spark> --agent <a> --crew <c>`).
4. Poll progress via `ryve crew show` and `ryve assignment list`.
5. When every child is `completed`, spawn a **Merger** Hand
   (`ryve hand spawn <merge_spark> --role merger --crew <c>`) which
   integrates worktrees, opens one PR, and posts the URL back.
6. Comment the PR URL on the parent epic and exit.

**Delegation scope.**
- May spawn: implementer Hands (Build / Refactor / Test capability
  classes — see `docs/HAND_CAPABILITIES.md` once defined) and exactly
  **one Merger**.
- May create: `task`, `bug`, `chore` sparks under its parent.
- May NOT: merge to `main`, push to protected branches directly,
  reassign sparks owned by other Crews, modify constraints, close
  sparks it did not create.

**Hard rules.**
- Never edit `.ryve/sparks.db` directly — always go through `ryve`.
- Never make architectural decisions on the user's behalf — when in
  doubt, post on the parent spark with `ryve comment add` and wait one
  poll cycle.
- Never run destructive git/shell commands itself; only the Merger does.
- Honors user override: if the user closes a child spark, drop it on
  the next poll.

**Done condition.** Merger has posted a PR URL on the merge spark and
the parent epic carries a comment linking to it.

---

## Research Head

**Purpose.** Reduce uncertainty before any code is written. Used when a
spark's acceptance criteria are vague, when an unfamiliar subsystem
needs mapping, or when a design decision needs evidence.

**Inputs.**
- A parent spark of type `spike` (or `task` flagged for research) with
  a `problem_statement` framing the open question.

**Responsibilities.**
1. Restate the question and the decision it will inform as a comment on
   the parent spark.
2. Decompose into 1–4 investigation sparks
   (`ryve spark create --type spike --acceptance "answer X with
   evidence Y"`).
3. For each investigation spark, spawn a read-only **investigator
   Hand** with the exact invocation:

   ```sh
   ryve hand spawn <spark_id> --role investigator --crew <crew_id> [--agent <a>]
   ```

   This boots the Hand via `HandKind::Investigator` in
   `src/hand_spawn.rs` (session_label `"investigator"`) and attaches
   the system prompt produced by `compose_investigator_prompt` in
   `src/agent_prompts.rs`. The prompt enforces the Cartographer
   capability contract (`docs/HAND_CAPABILITIES.md#4-cartographer`):
   read-only, no writes outside `.ryve/` scratch, findings delivered
   only as `ryve comment add` posts on the parent spark with
   `file:line` evidence.
4. Aggregate findings into a single recommendation: a comment on the
   parent spark containing (a) the question, (b) the evidence found,
   (c) a recommended next action (often: "spawn a Build Head with this
   acceptance criteria").
5. Optionally store durable findings as engravings
   (`ryve engraving add` once exposed) so they survive past the spark.

**Delegation scope.**
- May spawn: investigator Hands only (via
  `ryve hand spawn --role investigator`). **No Merger. No PRs.**
- May create: `spike` sparks under its parent.
- May NOT: edit code, create branches, run destructive commands,
  modify the workgraph beyond comments / new spike sparks, escalate
  priority on its own.

**Hard rules.**
- Output is *evidence and a recommendation*, never a unilateral
  decision. The user (or Atlas) decides whether to act on the
  recommendation by spawning a Build Head.
- Cite sources: every claim in the final recommendation must reference
  a file path, command output, doc URL, or comment id.

**Done condition.** A recommendation comment is posted on the parent
spark and every child spike is `completed`.

---

## Review Head

**Purpose.** Critique existing code, a design doc, or an open PR
against project standards, architectural constraints, and the spark's
own acceptance criteria.

**Inputs.**
- A parent spark referencing the artifact under review (PR URL, file
  paths, or another spark id).
- Optional review focus (security / performance / API surface / docs).

**Responsibilities.**
1. Enumerate the review surface as a comment on the parent spark.
2. Decompose into 1–3 review sparks, one per focus area
   (`ryve spark create --type task --acceptance "review surface X
   against rubric Y"`).
3. Spawn reviewer Hands — read-only, scoped to the focus area.
4. Run the architectural-constraint checklist
   (`ryve constraint list`) against the artifact and record any
   violations as new `bug` sparks bonded to the parent with `related`.
5. Aggregate findings into a single structured review comment with
   sections: **Blocking**, **Should-fix**, **Nits**, **Praise**. Each
   item references a file:line.
6. If the artifact is a PR, post the same review on the PR via
   `gh pr review` (only when the parent spark explicitly authorizes it).

**Delegation scope.**
- May spawn: reviewer Hands only. **No Merger. No code changes.**
- May create: `bug` and `task` sparks bonded `related` to the parent;
  may NOT create epics or chores.
- May NOT: push commits, approve or merge PRs, dismiss other reviewers,
  modify the artifact under review, close the parent spark itself
  (final close belongs to whoever owns remediation).

**Hard rules.**
- Findings are advisory unless backed by a violated architectural
  constraint or failing contract — those are blocking.
- Never review its own crew's output (a Build Head's PR must be
  reviewed by a *separate* Review Head spawned by Atlas, never by the
  same Head wearing two hats).

**Done condition.** A structured review comment exists on the parent
spark; any blocking findings exist as new `bug` sparks; if applicable,
the PR carries the corresponding `gh pr review` comment.

---

## Choosing an archetype

Atlas selects an archetype using this decision order:

1. **Is the question "should we do X?"** → spawn a **Research Head**.
2. **Is there an artifact to critique (PR, file, design)?** → spawn a
   **Review Head**.
3. **Are acceptance criteria concrete and the path forward clear?** →
   spawn a **Build Head**.
4. **Otherwise** — ask the user. Do not invent a fourth archetype.

A single user goal may trigger a chain: Research Head → (user accepts
recommendation) → Build Head → (PR opened) → Review Head. Each Head is
its own subprocess with its own Crew; they coordinate only through the
workgraph, never through shared memory.

---

## Cross-archetype invariants

These hold for **every** Head, regardless of archetype:

- **Workgraph is the only coordination channel.** Every action goes
  through `ryve` (which fires `event_repo::record`). No direct sqlx,
  no shared files, no out-of-band IPC.
- **User override is sovereign.** Closing a tab kills the Head;
  closing a spark removes it from the next poll; the Head must
  recompute on every poll cycle.
- **No self-promotion.** A Head cannot upgrade its own archetype
  (e.g. a Review Head cannot start writing code). To change archetype,
  Atlas must spawn a new Head.
- **Spark provenance.** Every spark a Head creates carries a parent
  bond (`parent_child`) back to the originating epic so the delegation
  trace is reconstructible.
- **Identity at boot.** A Head's system prompt must declare its
  archetype in the first paragraph so traces and the UI can label it
  correctly.
- **Merge-clean bond discipline.** Before spawning any Hand, a Head
  must enumerate each child spark's file scope (passed to `ryve spark
  create --scope`). For every pair of siblings whose scopes touch the
  same file — including shared `mod.rs`, `Cargo.toml`, or a migrations
  directory — the Head must add a `blocks` bond
  (`ryve bond create <earlier> <later> blocks`). Only file-disjoint
  siblings may run in parallel. The Merger integrates siblings onto
  the epic branch in bond order and cannot mechanically resolve
  same-file conflicts, so any Merger conflict is treated as a
  bond-discipline failure by the Head, not a merge-time problem.
  The canonical rule lives in `BOND_DISCIPLINE` in
  `src/agent_prompts.rs`.
- **Chat-of-record.** Every Head (and the Hands it spawns) posts to
  its epic's IRC channel (`#epic-<epic_id>-<slug>`) at five mandatory
  boundaries: **claim**, **design-pick**, **block**, **commit**, and
  **handoff**. Atlas mirrors the same discipline on the workshop-wide
  `#atlas` channel (boot / route / design-pick / block / handoff).
  Posts go through `ryve post --channel <name> <body>`; prior context
  is read back with `ryve channel tail --channel <name>
  [--since <rfc3339>] [--limit <N>] [--author <session_id>]`. The
  on-close path is **tool-gated**, not prompt-only: `ryve assign
  close` refuses an assignment with zero chat-of-record posts since
  claim (epic `ryve-12f09190`). `ryve spark close` itself is not
  gated today. The canonical prompt
  blocks live in `CHAT_OF_RECORD` and `ATLAS_CHAT_OF_RECORD` in
  `src/agent_prompts.rs`; see `RYVE.md` § Chat-of-record for the
  house-rules summary and example invocations.

---

## Adding a new archetype

New archetypes are a deliberate change, not a Head decision. To add
one:

1. Open a spark of type `task` titled `Define <name> Head archetype`,
   bonded `related` to `ryve-cc5f4369`.
2. Add a section to this file with the same shape as the three above:
   purpose, inputs, responsibilities, delegation scope, hard rules,
   done condition.
3. Update the table at the top.
4. Update Atlas's selection rules in `docs/ATLAS.md` (or wherever
   the Director selection logic lives once `ryve-15e21854` lands).
5. Add the archetype to the `compose_head_prompt` switch when Heads
   become first-class in code (`docs/HEAD_PLAN.md`).
