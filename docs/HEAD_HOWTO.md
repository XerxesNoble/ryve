# HOWTO: Add a new Head archetype

> Companion to [`HEAD_ARCHETYPES.md`](HEAD_ARCHETYPES.md) and
> [`AGENT_HIERARCHY.md`](AGENT_HIERARCHY.md). If you are not yet familiar
> with what a Head is, read those first.

Head archetypes are a **deliberate change**, not an ad-hoc decision a Head
makes at runtime. Atlas is explicitly forbidden from inventing a fourth
archetype on the fly — see the decision rules in
[`HEAD_ARCHETYPES.md#choosing-an-archetype`](HEAD_ARCHETYPES.md#choosing-an-archetype).

This HOWTO walks you through the full checklist for introducing a new one.
Skipping any step leaves the archetype half-wired: the docs may describe
it, but Atlas will never pick it, or worse — it will pick it and the Head
will boot with the wrong prompt.

## TL;DR

```
  1. Open a spark                 ryve spark create --type task …
  2. Add the archetype section    edit docs/HEAD_ARCHETYPES.md
  3. Update the top table         edit docs/HEAD_ARCHETYPES.md
  4. Update Atlas's selection     edit docs/ATLAS.md (or the live selector)
  5. Wire the prompt branch       edit src/agent_prompts.rs
  6. Update WORKSHOP.md template  edit data/src/agent_context.rs
  7. Add an archetype test        edit src/agent_prompts.rs tests
  8. Rebuild and verify           cargo build && ryve head --help
```

## Step 1 — Open a spark

Every new archetype starts as a workgraph spark so the decision is
reviewable. Bond it `related` to the archetype-catalog spark
(`ryve-cc5f4369` today).

```sh
ryve spark create --type task --priority 2 \
    --problem 'no archetype covers long-lived spike tracking' \
    --acceptance 'HEAD_ARCHETYPES.md has a Watch archetype section' \
    --acceptance 'compose_head_prompt has a Watch branch' \
    --acceptance 'ryve head --help lists watch in the archetype table' \
    'Define Watch Head archetype'

ryve bond create ryve-cc5f4369 <new_id> related
```

## Step 2 — Add the archetype section to `HEAD_ARCHETYPES.md`

Copy the shape of the existing sections (Build / Research / Review) and
fill in:

- **Purpose** — the single sentence that explains when Atlas should pick
  this archetype.
- **Inputs** — what the parent spark must contain for this archetype to
  make sense (e.g. a `spike` type, a PR URL, a performance budget).
- **Responsibilities** — a numbered list of what the Head does, from
  receiving the goal to posting the done signal. Every step must map to a
  concrete `ryve` CLI invocation so the prompt can reference it verbatim.
- **Delegation scope** — what kinds of Hands the Head may spawn, what
  kinds of sparks it may create, and (crucially) what it may **not** do.
  New archetypes almost always need a `may NOT` list longer than their
  `may` list. Borrow heavily from the existing sections.
- **Hard rules** — the non-negotiables that must show up verbatim in the
  Head's system prompt. If a rule isn't here, it's not enforced.
- **Done condition** — a single observable criterion that says "the epic
  is closed" for this archetype. Build Heads close via PR; Research Heads
  via a recommendation comment; Review Heads via a structured review.
  Your new archetype needs one just as specific.

## Step 3 — Update the top table

At the top of `HEAD_ARCHETYPES.md` there is a summary table. Add a row
for your archetype so operators can scan the catalog at a glance:

```md
| **YourName** | One-line purpose | Default crew shape | Closes spark by |
```

Keep the one-liner to a single clause. If you need two clauses, the
archetype probably overlaps with one of the existing three and you
should reuse that one instead.

## Step 4 — Update Atlas's selection rules

Atlas's decision tree for picking an archetype lives in `docs/ATLAS.md`
(section: *Selecting a Head*). Add a branch for the new archetype **in
priority order** — the rule at the top wins. The tree is read top-down
because Atlas reads it top-down. If your new archetype is a specialization
of an existing one, slot it *above* its parent; otherwise, slot it *below*
all three standard archetypes so it does not displace them.

If the live selector is ever moved into code (spark `ryve-15e21854` or a
follow-up), update the matching match arm there too. Until then, the
prompt is the selector.

## Step 5 — Wire the prompt branch

The Head system prompt is composed by `compose_head_prompt` in
[`src/agent_prompts.rs`](../src/agent_prompts.rs). Today it takes a single
workflow template. When you add a new archetype, refactor the signature
to take an `Archetype` enum and branch on it, OR add an archetype-specific
suffix block that gets appended after the shared preamble — whichever
keeps the diff smallest.

At minimum, your new archetype's prompt block must:

- **Name the archetype** in the first paragraph (cross-archetype invariant:
  *"Identity at boot — a Head's system prompt must declare its archetype
  in the first paragraph so traces and the UI can label it correctly"*).
- **List the hard rules** verbatim from Step 2.
- **Link the done condition** to a concrete `ryve` command the Head can run
  (e.g. `ryve spark close <id> completed`, `ryve comment add <id> …`).

## Step 6 — Update the `WORKSHOP.md` template

The Heads section in `.ryve/WORKSHOP.md` is generated by
`generate_workshop_md` in
[`data/src/agent_context.rs`](../data/src/agent_context.rs). Add your
archetype to the table in the `### Archetypes` section so every Hand and
Head that reads `WORKSHOP.md` sees the same catalog the docs describe.

Do **not** edit `.ryve/WORKSHOP.md` by hand — it is regenerated on every
workshop refresh and your edits will be overwritten.

## Step 7 — Add a regression test

`src/agent_prompts.rs` has unit tests that snapshot the composed prompt
and assert on its contents (e.g. `head_prompt_explains_workflow`). Add a
test for your new archetype that asserts:

- the archetype name appears in the first paragraph,
- each hard rule appears verbatim,
- the done-condition command appears.

These tests are the only thing that stops a future edit from silently
dropping a pillar of your archetype.

## Step 8 — Rebuild and verify

```sh
cargo build
cargo test -p data workshop_md_   # WORKSHOP.md generator tests
cargo test agent_prompts           # prompt regression tests

# Smoke test the help output
./target/debug/ryve head --help | grep -i <yourname>
```

If `ryve head --help` does not list your archetype, the docs updated
but the WORKSHOP.md generator (Step 6) did not. Go back to Step 6.

## Checklist

- [ ] Spark opened and bonded `related` to `ryve-cc5f4369`.
- [ ] New section in `docs/HEAD_ARCHETYPES.md` with Purpose / Inputs /
      Responsibilities / Delegation scope / Hard rules / Done condition.
- [ ] Top table in `docs/HEAD_ARCHETYPES.md` updated.
- [ ] Atlas selection rules in `docs/ATLAS.md` updated.
- [ ] `compose_head_prompt` branch added in `src/agent_prompts.rs`.
- [ ] Archetype table in `generate_workshop_md`
      (`data/src/agent_context.rs`) updated.
- [ ] New regression test in `src/agent_prompts.rs`.
- [ ] `cargo build && cargo test` green.
- [ ] `ryve head --help` lists the new archetype.
- [ ] Spark closed: `ryve spark close <id> completed`.
