# Spike: Optional Personalization of Atlas

**Spark:** ryve-fe7e1e42
**Parent epic:** ryve-5472d4c6 (Introduce Atlas as Ryve's Primary Director Agent)
**Status:** Spike — written exploration only, no code changes
**Date:** 2026-04-08

## Question

Can Atlas be user-renamable (e.g. "Pilot", "Maestro", "Ada") while preserving the
Director system role *without* weakening the semantics that make Atlas load-bearing
in Ryve's architecture?

## TL;DR

**Yes — if and only if we keep the system role and the display name as two distinct
identifiers, and treat the user-chosen name as a presentation-layer alias.** The
Director role must remain a stable, code-level constant; the display name is a
per-workshop preference that flows through copy, prompts, and UI but never through
routing, persistence keys, telemetry, or capability checks.

The risk is not the rename itself — it is the temptation to let the rename leak into
identity. The mitigation is a hard architectural seam between *role* and *persona*.

## What "Atlas" actually means today

From the parent epic and adjacent in-progress sparks (ryve-d772adfe, ryve-acdb248a,
ryve-4c6ca1b0), Atlas carries three overlapping meanings that personalization could
otherwise blur:

1. **Role** — `Director`, a first-class agent role alongside `Head` and `Hand`.
   This is a code-level enum/type that gates capabilities (delegation, synthesis,
   user-facing turn ownership).
2. **Instance** — the specific default Director agent that ships with every
   workshop. There is exactly one Director instance per workshop conversation
   context, and Atlas is its identity at install time.
3. **Persona** — the user-visible name, voice, and "feel" used in copy, prompts,
   greetings, transcripts, and the UI ("Ask Atlas…", "Atlas is delegating to…").

Today (1) and (3) are conflated because there is only one possible name. The spike
question is really: *can we cleanly separate (3) from (1) and (2)?*

## Proposed model

Three distinct identifiers, each with a clear scope:

| Identifier        | Example      | Scope          | Mutable? | Used by                                  |
| ----------------- | ------------ | -------------- | -------- | ---------------------------------------- |
| `role`            | `Director`   | code           | no       | routing, capability checks, traces, tests |
| `agent_id`        | `atlas`      | persistence    | no       | DB rows, event logs, telemetry, configs  |
| `display_name`    | `"Pilot"`    | presentation   | yes      | UI copy, system prompts, user transcripts |

- **`role`** is a Rust enum variant. It never appears in user copy and never changes.
- **`agent_id`** is the stable slug used as a primary key / foreign key everywhere
  state is written. Even if a user renames their Director to "Pilot", the row in
  `agents` is still `agent_id = "atlas"`. This protects history, replay, and any
  cross-workshop tooling.
- **`display_name`** is a per-workshop string preference, defaulting to `"Atlas"`.
  It is interpolated into prompts and UI at render time.

This is the same pattern Slack uses for users (immutable user id vs. mutable
display name) and Git uses for commits (immutable SHA vs. mutable refs). It is well
understood and survives rename without breaking referential integrity.

## What changes, and what does not

### Changes (presentation layer only)

- A `workshop_settings.director_display_name` field, defaulting to `"Atlas"`.
- A renderer that substitutes `{{director_name}}` in:
  - System prompts ("You are {{director_name}}, the Director of this workshop…")
  - UI strings ("Ask {{director_name}}", "{{director_name}} is thinking…")
  - User-visible transcript labels
- A settings affordance to change the name (validation: non-empty, length cap,
  no characters that break prompt templating).
- A migration: existing workshops get `director_display_name = "Atlas"` explicitly,
  so the default is materialized rather than implicit.

### Does NOT change (semantic core)

- The `Director` role enum and any `match` arms over it.
- The `agent_id = "atlas"` primary key in the agent registry.
- Routing: "top-level user requests route through the Director" remains true.
  The Director happens to be `agent_id = atlas`; the display name is irrelevant.
- Delegation contracts (ryve-4c6ca1b0): contracts are between roles, not personas.
- Telemetry / traces / event logs: continue to record `agent_id` and `role`, never
  the display name. This is critical for debuggability across renames and across
  workshops.
- Internal docs and ADRs: continue to say "Atlas (Director)". The canonical name
  is Atlas; "Pilot" is a user's hat on top.

## Why this preserves system semantics

The semantics that the parent epic cares about are:

1. **There is a single, stable, user-facing primary agent.** Preserved — there is
   still exactly one Director per workshop. The user just gets to call it whatever
   they want. Singularity is a property of the role slot, not the name.
2. **Delegation chains have a coherent mental model.** Preserved — the chain is
   `Director → Head → Hand`, and the Director's identity in code/logs/traces is
   still `atlas`. A user reading their own transcript sees "Pilot delegated to the
   File Head", but the developer reading the trace sees `director(atlas) →
   head(file)`. Both views are coherent; neither weakens the other.
3. **Product copy/UI/debugging share a model.** Preserved with one nuance: *user
   copy* gets personalized, but *debugging surfaces* (logs, traces, ADRs, error
   messages aimed at developers) stay on `atlas`/`Director`. This is the right
   split — debuggers need stability, users want warmth.
4. **Atlas defaults to handling user requests.** Preserved — routing is by role,
   not name.

The semantic weakening we are *avoiding* is the failure mode where rename leaks
into the data layer or routing layer. If `agent_id` itself were mutable, two
workshops with renamed Directors could not share state, telemetry would fragment,
and any cross-workshop "what is the Director currently doing?" query would need a
name-resolution step. By keeping `agent_id` immutable, none of that happens.

## Failure modes considered

- **System prompt drift.** If the substituted name appears in a prompt that also
  hardcodes "Atlas" elsewhere, the model sees two names and gets confused.
  *Mitigation:* every prompt template that mentions the Director uses
  `{{director_name}}` exclusively; lint/test for the literal string `Atlas` inside
  templates.
- **User picks an offensive or breaking name.** *Mitigation:* validation
  (length, charset, no template-injection characters). Out of scope to enforce
  taste.
- **Cross-workshop confusion.** A user with two workshops, one named "Pilot" and
  one named "Maestro", might not realize they are the same underlying agent role.
  *Mitigation:* tooltips / settings page note "This is your Director (Atlas).
  You've renamed it to Pilot in this workshop." This makes the layering explicit.
- **Logs/traces become unreadable.** This would happen *if* we logged
  `display_name`. We do not. Logs always show `atlas`. Resolved by construction.
- **Tests grow brittle.** Tests that match on UI strings would break under rename.
  *Mitigation:* tests that care about Director behavior assert on role/agent_id;
  tests that care about user copy use the configured display name from a fixture.
- **Migration inertia.** Future agent-system features (e.g. multi-Director
  experiments) might assume `agent_id = atlas` is the only Director. *Mitigation:*
  this is already the assumption today; the rename does not change it. If we ever
  want pluggable Director instances, that is a different epic.

## Non-goals of this spike

- Implementing the rename UI or settings page.
- Choosing the exact column name / migration shape (left to the implementer).
- Multi-Director or per-conversation Director swapping. The role is still a
  singleton per workshop.
- Renaming Heads or Hands. The same pattern would work, but Heads/Hands are
  typically referred to by their function ("File Head", "Test Hand"), which is
  already a kind of role-based naming. Personalization there has lower value.
- Localization. `display_name` is a single string, not a translation table. If
  Ryve later needs localized agent names, that is an additive change on top of
  this model.

## Recommendation

**Proceed, but only after the parent epic (ryve-5472d4c6) has landed Atlas as the
canonical Director with a stable `agent_id` and a `Director` role enum.** Without
those seams, there is nothing to separate the persona *from*, and a rename feature
would be the thing that *introduces* the conflation we are trying to avoid.

Concretely, the prerequisite chain is:

1. ryve-d772adfe (role model with Director/Head/Hand) — establishes `role`.
2. ryve-acdb248a (route top-level requests through Atlas) — establishes the
   routing-by-role contract.
3. ryve-4c6ca1b0 (delegation contracts) — establishes that contracts are between
   roles, not names.
4. *Then* a follow-up spark can add `director_display_name` as a presentation-only
   preference, with the guardrails listed above.

If those prerequisites are honored, optional personalization is a small,
low-risk, additive change that meaningfully improves the product feel without
weakening any semantic the Director role exists to provide.

## Open questions for the implementer

- Should `display_name` live on the workshop config, or on the agent row keyed by
  `agent_id`? (Spike leans: workshop config, because it is a per-workshop
  preference and the agent row should stay portable.)
- Should the rename be surfaced in the conversation itself ("You can call me
  Pilot from now on") or only via settings? (Out of scope; product call.)
- Do we want a "reset to Atlas" affordance? (Recommended — cheap, and it makes
  the canonical name discoverable.)
