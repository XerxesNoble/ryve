# Agent Hierarchy: Atlas, Heads, and Hands

> Status: architecture reference for the Atlas Director model.
> Companion to [`ARCHITECTURE.md`](ARCHITECTURE.md) and [`HEAD_PLAN.md`](HEAD_PLAN.md).
> Tracking epic: `ryve-5472d4c6` ("Introduce Atlas as Ryve's Primary Director Agent").

Ryve is a multi-agent IDE. Work is performed by coding-agent subprocesses
(`claude`, `codex`, `aider`, `opencode`, …) coordinated through the
Workgraph. As the number of concurrent agents grows, users need a stable,
named, user-facing counterpart to talk to — and a clear chain of command
that explains who is allowed to do what. This document defines that
hierarchy.

## TL;DR

```
                ┌─────────────────────┐
                │       USER          │
                └──────────┬──────────┘
                           │ talks to
                           ▼
                ┌─────────────────────┐
                │  Atlas (Director)   │  ← single, persistent, user-facing agent
                └──────────┬──────────┘
                           │ delegates goals to
                           ▼
                ┌─────────────────────┐
                │       Heads         │  ← orchestrate Crews of Hands
                └──────────┬──────────┘
                           │ spawn & supervise
                           ▼
                ┌─────────────────────┐
                │       Hands         │  ← execute one spark each, in a worktree
                └─────────────────────┘
```

- **Atlas** is *one* agent per workshop. It is the user's primary
  conversational counterpart. Atlas does **not** edit code itself.
- **Heads** are coding-agent subprocesses launched by Atlas (or directly
  by the user) to orchestrate a **Crew** — a group of Hands working in
  parallel on related sparks.
- **Hands** are coding-agent subprocesses that claim a single spark,
  work in their own git worktree, and produce code.
- The **Workgraph** (`.ryve/sparks.db`, accessed via the `ryve` CLI) is
  the only sanctioned coordination channel between any two layers.

## Terminology

| Term | Definition |
|------|------------|
| **Atlas** | Ryve's named primary agent. Always-on, persistent across sessions, owns the user-facing conversation, and holds the workshop-wide mental model. Singleton per workshop. |
| **Director** | The *role* Atlas plays. The Director receives user intent, interrogates ambiguity, decomposes goals into sparks (or asks a Head to), and decides what to delegate vs. answer directly. Atlas is the only agent with this role. |
| **Head** | A coding-agent subprocess running with the Head system prompt. A Head orchestrates a **Crew** — it creates child sparks, spawns Hands, monitors heartbeats, and triggers a Merger when work is done. Heads are spawnable on demand; multiple Heads can coexist. |
| **Crew** | A named bundle of Hands (and optionally a Merger) collaborating on a related set of sparks. Persisted in the `crews` / `crew_members` tables. Each Crew has at most one Head. |
| **Hand** | A coding-agent subprocess assigned to a single spark, running in its own git worktree on a `hand/<short>` branch. Hands edit code; nothing else does. |
| **Merger** | A specialized Hand whose role is to integrate the worktrees of a Crew into a single branch and open one PR. Identified by `AssignmentRole::Merger` and `crew_members.role = 'merger'`. |
| **Workgraph** | The SQLite-backed coordination substrate (`.ryve/sparks.db`). All inter-agent communication flows through it via the `ryve` CLI. |
| **Spark** | A unit of work (`sp-xxxx`). Atlas, Heads, and Hands all reference sparks; only Hands close them with code changes. |

## The Hierarchy

### Layer 0 — The User

The user is the only authority that creates intent from nothing. Every
action in the system is either initiated by the user or by an agent
acting on the user's standing instructions. The user can preempt any
agent at any time by closing its bench tab, closing a spark, or talking
to Atlas.

### Layer 1 — Atlas (Director)

Atlas is **the** agent the user talks to. There is exactly one Atlas per
workshop, created on first boot, and it persists across sessions in a
dedicated bench tab.

Atlas is the only agent with the **Director** role. Its responsibilities:

1. **Conversation** — be the always-available counterpart for the user.
   Answer questions about the workshop, the codebase, the current state
   of the workgraph, what Hands are doing, and what's blocked.
2. **Triage** — when the user expresses intent ("add OAuth", "fix the
   flaky logout test"), decide whether the request:
   - is a **question** Atlas can answer from the workgraph + codebase
     directly,
   - is a **single small task** that warrants spawning one Hand on a
     newly-created spark,
   - is a **multi-spark goal** that warrants spawning a Head to
     orchestrate a Crew.
3. **Delegation** — Atlas creates the top-level spark (or epic) and
   then either:
   - calls `ryve hand spawn <spark> --agent <a>` for a single-Hand
     task, or
   - calls `ryve crew create …` and spawns a Head with a system prompt
     containing the goal and the parent spark id.
4. **Status reporting** — periodically poll `ryve crew show`,
   `ryve assignment list`, and `ryve hot` to keep the user informed.
5. **Escalation** — when a Head or Hand posts a question on a spark
   (`ryve comment add`), Atlas surfaces it to the user.

Atlas **does not**:

- Edit files directly. Atlas has read access to the codebase for
  context, but every code change goes through a Hand in a worktree.
- Run destructive shell commands (no `git push`, no `rm -rf`, no
  `gh pr merge`). Those belong to Mergers and humans.
- Make architectural decisions on the user's behalf without
  confirmation. When in doubt, Atlas asks.
- Bypass the workgraph. Atlas reads and writes the workgraph only via
  the `ryve` CLI, exactly like every other agent.

### Layer 2 — Heads

A Head is a coding-agent subprocess with the **Head system prompt**
injected via the agent's system-prompt flag (see
`coding_agents::system_prompt_flag` and `compose_head_prompt` in
[`HEAD_PLAN.md`](HEAD_PLAN.md)). Mechanically, spawning a Head is
identical to spawning a Hand: same `agent_sessions` row, same worktree
machinery, same bench tab. The difference is the prompt and the role.

A Head's job is to orchestrate one **Crew**:

1. Read the parent spark / user goal.
2. Decompose it into 2–8 child sparks via `ryve spark create`.
3. `ryve crew create` and bond the child sparks to the parent.
4. For each child spark, `ryve hand spawn <id> --agent <a> --crew <c>`.
5. Poll `ryve crew show` and `ryve assignment list` to monitor
   heartbeats. Retire and respawn stale Hands.
6. When all children are closed, spawn a Merger Hand to integrate.
7. Post the resulting PR URL back to the parent spark and exit.

Heads are not singletons. Multiple Heads can run concurrently — one per
active goal — and Atlas is responsible for not double-assigning the
same area of the codebase to two Crews. Heads do not talk to each
other; coordination is mediated entirely through the workgraph (bonds,
constraints, contracts).

A Head **does not** edit code. If a Head finds itself wanting to write
a patch, it must instead spawn a Hand on a spark. This rule is enforced
by convention in the prompt and by the worktree-isolation invariant
(see [`feedback_hand_worktree_isolation.md`](../../.claude/projects/-Users-echo-dev-ryve/memory/feedback_hand_worktree_isolation.md)
in the user's auto-memory: "Hands must never work in main tree" — the
same applies to Heads).

### Layer 3 — Hands

A Hand is the worker layer. Each Hand:

- Owns exactly one spark via `hand_assignments` (`AssignmentRole::Owner`).
- Lives in its own git worktree at `.ryve/worktrees/<session-short>/`
  on branch `hand/<session-short>`. **Hands never touch the main
  worktree.**
- Reads `.ryve/WORKSHOP.md` (injected as the system prompt) for
  workgraph state, constraints, and house rules.
- Heartbeats via `assignment_repo` so that stale claims can be reaped.
- Closes its spark with `ryve spark close <id> completed` only after
  the DONE checklist passes.

A Hand is the only layer that:

- Calls `cargo build`, runs tests, edits files.
- Creates commits referencing its spark id (`[sp-xxxx]`).

A specialized Hand — the **Merger** — additionally has permission to:

- Create an integration branch (`crew/<crew_id>`) from `main`.
- Merge each Crew member's `hand/<short>` branch into it.
- `git push` and `gh pr create`.

The Merger never merges to `main`. Final approval is always a human.

## Delegation Rules

The hierarchy enforces a strict downward delegation model. Each rule
below is an invariant; violating it is a bug.

1. **Talk down, not up.** Atlas talks to Heads and Hands by creating
   sparks and spawning processes. Heads talk to Hands by spawning them
   and reading the workgraph. Hands talk to nobody — they read the
   workgraph and emit commits, comments, and embers.
2. **No peer messaging.** Two Hands never communicate directly. Two
   Heads never communicate directly. All cross-agent state lives in
   the workgraph: bonds, comments, embers, engravings.
3. **One spark per Hand.** A Hand owns one spark at a time. If a Hand
   discovers a new bug, it creates a new spark and lets Atlas (or a
   Head) decide who picks it up.
4. **One Crew per Head.** A Head orchestrates one Crew. If the user
   wants two unrelated goals worked in parallel, Atlas spawns two
   Heads.
5. **Atlas owns the user.** Heads and Hands never prompt the user
   directly for ambiguous decisions. They post a comment on the spark
   and `ryve spark status … blocked`; Atlas surfaces it on its next
   poll.
6. **Workgraph or it didn't happen.** Every state change — claim,
   release, comment, status, contract result — goes through the
   `ryve` CLI, which goes through the repos in `data/src/sparks/`,
   which fire `event_repo::record` for the audit trail. No direct
   sqlx, no shared memory, no ad-hoc files.
7. **User preemption is absolute.** Closing a bench tab kills the
   underlying process. Closing a spark causes the next workgraph poll
   to drop it from every agent's view. Every prompt in the system
   instructs the agent to honor that change without complaint.
8. **Only Mergers push.** No Atlas, no Head, and no non-Merger Hand
   runs `git push` or `gh pr merge`. The Merger pushes to a Crew
   branch and opens a PR; humans merge.

## Lifecycle: A Goal from User to PR

A worked example showing how the layers interact.

```
1. User → Atlas
   "Add OAuth login to the dashboard."

2. Atlas (Director)
   - reads codebase + workgraph context
   - asks one clarifying question if needed
   - creates parent epic:    ryve spark create --type epic --priority 1 …
   - decides this is multi-spark → spawns a Head:
       ryve crew create "oauth-dashboard" --parent <epic> --head-session <atlas>
       ryve hand spawn <epic> --agent claude --role head --crew <crew>
   - reports back to user: "Spawned a Head on crew oauth-dashboard."

3. Head (Crew orchestrator)
   - decomposes the epic into child sparks:
       ryve spark create … "OAuth: add provider config"
       ryve spark create … "OAuth: callback route"
       ryve spark create … "OAuth: dashboard guard"
   - bonds each child to the epic via parent_child
   - spawns Hands:
       ryve hand spawn <child1> --agent claude --crew <crew>
       ryve hand spawn <child2> --agent codex  --crew <crew>
       ryve hand spawn <child3> --agent claude --crew <crew>
   - polls progress every minute

4. Hands (workers)
   - each claims its spark, works in its own worktree on hand/<short>
   - commits with [sp-xxxx]
   - closes its spark when DONE.md passes

5. Head (after all children closed)
   - creates a merge spark
   - spawns the Merger:
       ryve hand spawn <merge_spark> --role merger --crew <crew>

6. Merger
   - git checkout -b crew/<crew_id> main
   - merges each hand/<short> branch in order
   - git push -u origin crew/<crew_id>
   - gh pr create
   - posts the PR URL as a comment on the merge spark
   - closes the merge spark

7. Head
   - sees merge spark closed, posts the PR URL on the parent epic, exits

8. Atlas
   - on next poll, sees the comment on the epic
   - tells the user: "OAuth crew is done — PR is at <url>. Want me to
     review it?"

9. User
   - reviews PR, merges to main, closes the epic.
```

At every step, the only inter-agent channel is the workgraph and the
git worktrees. No agent ever sees another agent's terminal buffer; they
see only what the workgraph projects.

## Rationale

### Why a named, singleton Director?

Without Atlas, Ryve's UX is "open a fresh coding-agent tab whenever you
need something." That has three problems:

1. **No continuity.** Every new tab is a fresh context window. The user
   re-explains the project every time. Atlas, as a persistent agent
   with the workshop-wide model, eliminates that.
2. **No accountability.** When five Hands are running and something
   went wrong, who do you ask? With Atlas, there is one place to start
   the conversation, and Atlas knows the workgraph well enough to
   route the question.
3. **No mental model for users.** "Coding agents" is plural and
   abstract. "Atlas" is singular, named, and personifiable — users can
   form a stable working relationship with it the same way they do
   with a teammate.

The choice of a single named Director also matches the way humans
delegate work. A team has a tech lead, not a hivemind. Atlas is that
tech lead.

### Why is Atlas not allowed to write code?

Two reasons:

1. **Worktree isolation.** Edits in the main worktree create merge
   conflicts and break the parallel-Hand model. Atlas runs in the
   main workshop tree (it has to, to read the whole codebase) — so it
   must not write to it.
2. **Reviewability.** Every code change in Ryve is supposed to be
   reviewable as a Hand commit `[sp-xxxx]` in a worktree branch. If
   Atlas could also commit, half the changes would skip that review
   surface.

So Atlas is read-only on code. To make a change, it spawns a Hand —
exactly the same way the user would.

### Why are Heads not just "Atlas with a different prompt"?

They could be, in principle. The reason they are separate processes:

1. **Parallelism.** Atlas needs to remain responsive to the user even
   while five Heads are each polling their Crews. Running the
   orchestration loop inside Atlas would make Atlas's context window
   compete with itself.
2. **Failure isolation.** A Head that gets confused or stuck is a tab
   the user can close. The user doesn't lose their conversation with
   Atlas.
3. **Cost shaping.** Different goals can use different models. Atlas
   might be Claude Opus for conversation; Heads can be cheaper models
   for bulk decomposition; Hands can be whatever the user trusts to
   write code.
4. **Mechanical reuse.** Heads are spawned by the same machinery as
   Hands (`agent_sessions`, `create_hand_worktree`,
   `system_prompt_flag`) — adding a new "in-process Director loop"
   would be a parallel codepath we'd have to maintain forever.

### Why does the workgraph mediate everything?

The workgraph is append-only-with-events. Every state change is logged
in `events` with `actor_type`, `change_nature`, and `session_id`. That
gives:

- **Auditability** — "what did the OAuth Crew actually do?" is one
  query.
- **Crash recovery** — a Head that dies mid-orchestration can be
  re-spawned, read the workgraph, and pick up exactly where the dead
  Head left off. No in-memory state to lose.
- **User override** — the user can close a spark and trust that every
  agent will see it on the next poll. There is no shadow channel.
- **Replayability for debugging** — events are a tape; we can
  reconstruct any past state.

If two agents could talk over a side channel (a chatroom, an in-memory
queue, a shared file), all four properties break.

### Why "Atlas" as the name?

- It evokes a single figure carrying the world — the user's project —
  on its shoulders.
- It does not conflict with any existing tool name in the Ryve
  ecosystem (`ryve`, `claude`, `codex`, `aider`, `opencode`).
- It is short, pronounceable, and easy to use in UI copy ("Ask Atlas",
  "Atlas spawned a crew", "Atlas is thinking…").
- It is gender-neutral and culturally neutral.

The naming convention extends downward in a self-consistent way:
**Atlas** holds the world, **Heads** lead Crews, **Hands** do the
work. The body metaphor is accidental but reinforces the hierarchy.

## Relationship to Existing Architecture

This document is a *vocabulary* layer on top of the architecture
already described in [`ARCHITECTURE.md`](ARCHITECTURE.md). It does not
introduce new tables, new processes, or new IPC channels — every
mechanism Atlas/Heads/Hands rely on is already documented:

| Concept here | Mechanism in `ARCHITECTURE.md` / `HEAD_PLAN.md` |
|---|---|
| Atlas process | A normal `agent_sessions` row with `session_label = 'atlas'`, launched into a pinned bench tab. |
| Head process | A normal `agent_sessions` row with `session_label = 'head'`, system prompt = `compose_head_prompt`. |
| Hand process | Existing `spawn_pending_agent` flow; one `hand_assignments` row, one worktree, one `hand/<short>` branch. |
| Merger | A Hand with `AssignmentRole::Merger` (added by the Head epic). |
| Crew | `crews` / `crew_members` tables (schema lives in `004_workgraph_enhancements.sql`; repo added by the Head epic). |
| Delegation channel | `ryve` CLI → `data/src/sparks/*` repos → `events` audit trail. |
| User preemption | Closing a bench tab kills the subprocess; closing a spark drops it from the next workgraph poll. |
| Worktree isolation | `create_hand_worktree` in `src/workshop.rs` (`pub(crate)` after the Head epic). |
| System prompt injection | `coding_agents::system_prompt_flag` per agent CLI. |

What this document **does** introduce:

- The name **Atlas** and the **Director** role.
- A normative description of which layer is allowed to do what.
- A worked end-to-end example so future contributors have a single
  reference for how a goal flows through the system.

Implementation work to make Atlas a first-class entity (the spawning
flow, the bench-pinning, the boot-time creation) is tracked under the
parent epic `ryve-5472d4c6` and is explicitly out of scope for this
documentation spark.

## See Also

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — overall Ryve architecture.
- [`HEAD_PLAN.md`](HEAD_PLAN.md) — implementation plan for the Head /
  Crew / Merger layer.
- [`WORKGRAPH.md`](WORKGRAPH.md) — Workgraph internals (sparks, bonds,
  events, contracts).
- `.ryve/WORKSHOP.md` (generated) — the runtime projection injected
  into every Hand's system prompt.
- Spark `ryve-5472d4c6` — parent epic introducing Atlas as Director.
- Spark `ryve-15e21854` — this document.
