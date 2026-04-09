# Atlas — Default User Request Routing

> Spark ryve-acdb248a · part of the Atlas Director epic ryve-5472d4c6.

## TL;DR

**All top-level user-originated requests in Ryve route through Atlas by
default.** Atlas is Ryve's primary user-facing **Director** agent. It is
conversational, never edits source code itself, and delegates downward to
Heads (multi-spark goals) or Hands (single sparks) via the `ryve` CLI.

```
                User
                  │
                  ▼
              ┌───────┐
              │ ATLAS │   ← Director (default entry point)
              └───┬───┘
                  │ delegates
        ┌─────────┴─────────┐
        ▼                   ▼
     ┌──────┐            ┌──────┐
     │ HEAD │            │ HAND │
     └──┬───┘            └──────┘
        │ orchestrates
        ▼
     ┌──────┐
     │ HAND │ × N (Crew)
     └──────┘
```

## Routing Rules

1. **Default path**: `+` dropdown → **Ask Atlas...** (the first/primary
   item). This launches a coding-agent subprocess with the Atlas Director
   system prompt (`agent_prompts::compose_atlas_prompt`). The user types
   their request in the resulting bench tab; Atlas classifies the intent
   and delegates.

2. **Atlas decides who does the work** based on the user's request:
   - *Question or clarification* → answer directly, no delegation.
   - *Concrete spark* the user named → `ryve hand spawn <spark_id>`.
   - *High-level goal* needing decomposition → spawn a Head with an epic
     so the Head fans out a Crew.

3. **Atlas never edits code itself.** This is a hard rule baked into the
   Director system prompt and enforced by convention — Atlas only drives
   the `ryve` CLI.

## Bypass Paths (Advanced Flows)

The bench `+` dropdown exposes three explicit bypass options under the
`Bypass Atlas` section. These exist so power users can skip the Director
when they already know exactly what they want:

| Dropdown entry      | When to use it                                          |
|---------------------|---------------------------------------------------------|
| **New Hand...**     | You already know which spark to claim and which agent to use. Opens the spark + agent picker and runs the existing `spawn_pending_agent` flow. |
| **New Head...**     | You want a Crew of Hands but want to pick the agent / parent epic yourself instead of letting Atlas route the request. |
| **New Terminal...** | You want a plain shell tab with no agent and no routing layer at all. |

These paths bypass `compose_atlas_prompt` entirely. They are documented
here as a deliberate escape hatch — Atlas is the **default**, not the
**only** way to start work in a workshop.

## Implementation Map

| Concern              | Where it lives                                          |
|----------------------|---------------------------------------------------------|
| Director system prompt | `src/agent_prompts.rs::compose_atlas_prompt`          |
| Bench dropdown entry   | `src/screen/bench.rs` — `Message::FocusAtlas` (focuses pinned tab) |
| Auto-spawn (pinned)    | `src/main.rs::App::spawn_atlas_pinned` (on workshop open) |
| Spawn handler          | `src/main.rs::App::spawn_atlas`                       |
| Session label          | `agent_sessions.session_label = "atlas"`              |

## Related Sparks

- ryve-5472d4c6 — Parent epic: *Introduce Atlas as Ryve's Primary Director Agent*
- ryve-acdb248a — *Route top-level user requests through Atlas as default entry point* (this spark)
- ryve-1e3848b6 — Delegation trace model with Atlas as origin
- ryve-9972f264 — Atlas behavioural / prompting layer (refines `compose_atlas_prompt`)
- ryve-7aa4dcd8 — UX copy and chat identity for Atlas
- ryve-15e21854 — Architecture doc for Atlas / Director model
- ryve-fe7e1e42 — Optional personalization spike
