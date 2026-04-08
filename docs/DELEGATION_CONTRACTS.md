# Delegation Contracts: Director ↔ Head ↔ Hand

> Status: stubbed. Wire format and validation are implemented in
> `src/delegation.rs`. Concrete transports (CLI dispatch, subprocess
> spawn, comment writes) are layered on top in follow-up sparks under
> the Atlas epic (`ryve-5472d4c6`).

Ryve runs three nested layers of LLM-powered coding agents. Each layer
talks to the layer above and below it through a small set of stable,
serializable payloads called **delegation contracts**. This document is
the human-readable spec for those contracts; the canonical machine
definitions live in `src/delegation.rs`.

```text
  Director (Atlas) ─── DirectorBrief ───▶ Head
                                            │
                                            │ HeadAssignment
                                            ▼
                                          Hand
                                            │
                                            │ HandReturn
                                            ▼
                                          Head
                                            │
                                            │ HeadSynthesis
                                            ▼
                                        Director
                                            │
                                            ▼
                                          User
```

## Why explicit contracts?

Every delegation in Ryve crosses a process boundary:

- Director → Head is a subprocess spawn (`ryve hand spawn` with the Head
  system prompt) plus a JSON brief delivered via the agent's initial
  prompt or a comment on the parent epic.
- Head → Hand is another subprocess spawn (`ryve hand spawn` with the
  Hand system prompt and `--crew`).
- Hand → Head is a comment posted on the spark plus the spark's closed
  status — the Head reads both via `ryve crew show` / `ryve spark show`.
- Head → Director is a comment posted on the parent epic plus the epic's
  closed/blocked status.

If we let the CLI argument list or the prompt text be the only source
of truth, callers can't reason about delegations without shelling out.
Defining contracts as plain Rust structs with serde derives lets the
Director and Head reason about delegation flows in-process, validate
them before crossing the boundary, and replay them in tests.

## The four contracts

### 1. `DirectorBrief` — Director → Head

The Director hands a brief to a freshly-spawned Head whenever it
decides a user request is large enough to warrant a Crew. The brief is
the *what* and *why* — never the *how*. The Head reads the workgraph,
decomposes the goal into sparks, and picks agents per sub-task on its
own.

| Field | Required | Purpose |
|---|---|---|
| `brief_id` | yes | Stable id assigned by the Director so the eventual `HeadSynthesis` can be correlated back. |
| `user_goal` | yes | Plain-language statement of what the user wants accomplished. |
| `parent_epic_id` | no | Existing epic to attach all created child sparks to. If absent, the Head must create its own parent epic. |
| `constraints` | no | Constraints propagated from the user or workshop ("don't touch billing", "must ship by Friday"). Mandatory inputs to the Head's decomposition. |
| `non_goals` | no | Things the user explicitly does NOT want done. Mirrors a spark's non-goals so the Head does not over-scope. |
| `success_criterion` | no | Plain-language definition of done. The Head MUST encode this as one or more `--acceptance` flags on the parent epic. |

**Validation:** `brief_id` and `user_goal` must be non-empty.

### 2. `HeadAssignment` — Head → Hand

A Head sends a `HeadAssignment` to a Hand when it spawns one via
`ryve hand spawn`. The fields map directly onto the CLI args plus the
system-prompt content composed by `agent_prompts::compose_hand_prompt`.

| Field | Required | Purpose |
|---|---|---|
| `spark_id` | yes | Spark the Hand will execute. |
| `crew_id` | no | Crew the Hand is enrolled in. `None` is permitted only for solo Hands spawned outside any Crew (the manual "+ → New Hand" UI path). |
| `agent_command` | yes | Coding agent CLI to invoke (`claude`, `codex`, `aider`, `opencode`). |
| `role` | yes | `hand` or `merger`. Distinguishes ordinary Hands from a Crew's Merger. |
| `origin_brief_id` | no | The `DirectorBrief.brief_id` this assignment derives from, propagated for traceability. |

**Validation:** `spark_id` and `agent_command` must be non-empty.

### 3. `HandReturn` — Hand → Head

A Hand sends a `HandReturn` back when it finishes (or gives up on) its
spark. Today the canonical channel is a comment posted on the spark
plus the spark's closed status; this struct is the schema of that
comment payload so the Head can parse it programmatically.

| Field | Required | Purpose |
|---|---|---|
| `spark_id` | yes | Spark the Hand was working on. |
| `session_id` | yes | `agent_sessions.id` of the Hand reporting in. |
| `outcome` | yes | `completed` / `blocked` / `declined` / `abandoned`. |
| `summary` | yes | Short human-readable summary; becomes the body of the comment. |
| `follow_up_sparks` | no | Spark ids the Hand discovered while working that the Head should schedule as new work. |
| `artifacts` | no | Git artifacts produced (commit shas, branch names, PR URLs). The Merger uses these to know what to integrate. |

**Validation:** `spark_id` and `session_id` must be non-empty.

### 4. `HeadSynthesis` — Head → Director

A Head sends a synthesis back once its Crew has finished (or partially
finished) the brief. The Director uses it to compose the user-facing
reply. The shape is intentionally narrow: one overall outcome, the
per-spark roll-up, and one summary string. The Director — not the
Head — owns the user-facing rendering.

| Field | Required | Purpose |
|---|---|---|
| `brief_id` | yes | The `DirectorBrief.brief_id` this synthesis answers. |
| `crew_id` | yes | Crew the Head was running. |
| `overall_outcome` | yes | Aggregate outcome derived from the constituent `HandReturn`s. |
| `summary` | yes | One short paragraph the Director can relay to the user verbatim. |
| `hand_returns` | no | Per-Hand returns in execution order so the Director can render a chronological recap. |
| `pr_url` | no | PR URL produced by the Crew's Merger, if any. |
| `escalations` | no | Sparks the Head escalates back to the Director as still requiring human input (blocked, declined, or follow-ups too large for the Crew). |

**Validation:** `brief_id` and `crew_id` must be non-empty.

## Outcomes

Both `HandReturn.outcome` and `HeadSynthesis.overall_outcome` are
drawn from the same closed enum:

| Outcome | Meaning |
|---|---|
| `completed` | All acceptance criteria satisfied; spark closed `completed`. |
| `blocked` | Work could not proceed; details captured in the summary; spark still open or marked `blocked`. |
| `declined` | Recipient explicitly refused the assignment (out of scope, duplicate, ambiguous). Caller should re-plan. |
| `abandoned` | Recipient stopped reporting heartbeats and was reaped by the caller. Caller should re-spawn. |

## Wire format

All four contracts derive `serde::Serialize` and `serde::Deserialize`
and are transported as JSON. Field naming follows Rust `snake_case`
conventions (no `rename_all` overrides). The matching encode helpers
in `src/delegation.rs` are:

- `delegate_to_head(&DirectorBrief)`
- `delegate_to_hand(&HeadAssignment)`
- `return_to_head(&HandReturn)`
- `synthesise_for_director(&HeadSynthesis)`

Each helper validates required fields and returns either the JSON
string or a `DelegationError`. Concrete transports — CLI dispatch,
subprocess spawn, comment writes — will replace the bodies of these
helpers in follow-up sparks without changing call sites.

## Stability

The contracts above are part of the public delegation API between
agents in different processes. Adding new optional fields is a
backwards-compatible change. Renaming fields, removing fields, or
changing required-field validation is a breaking change and must be
gated on a new spark.
