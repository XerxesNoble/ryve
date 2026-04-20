# Release 0.3.0 — Channel Projection + Chat-of-Record + Merge-Hand Contract

**Release ID:** `rel-2259eb66`
**Branch:** `release/0.3.0`
**Date:** 2026-04-20
**Main merge commit:** `bf0d6cd3`

## What was implemented

0.2.x made the workshop observable; 0.3.0 makes it **navigable** and **durable**. Three epics ship together: agents now post a durable chat-of-record to per-epic IRC channels and read each other's history on re-claim or boot; the UI renders those channels as filtered, searchable projections; and a new Merge-Hand role owns the full Epic → main integration lifecycle with a precondition-gated spawn and conflict handoff that respects the transition state machine.

### Chat-of-record — mandatory agent posting + #atlas history + sudden-death recovery (`ryve-12f09190`, PR #54, commit `cca8420`)

Six member sparks:

- **`ipc/src/chat_of_record.rs`** — foundation. `post_message(pool, NewPost { channel, body, author_session_id, epic_id })` writes to `irc_messages` and returns the row id; `tail(pool, TailFilter)` reads back filtered by channel / since / author with a `TAIL_MAX_LIMIT` of 1000 rows and a `TAIL_DEFAULT_LIMIT` of 50. The DB row is the ENTIRE durability contract — this module does not emit on the IRC wire (a separate 0.4.0 candidate).
- **`src/cli.rs`** — `ryve post --channel <name> [--author <session_id>] <body>` and `ryve channel tail --channel <name> [--since <ts>] [--limit <N>] [--author <session_id>] [--json]`. Default author is `$RYVE_HAND_SESSION_ID` when set so Hands are attributable without each caller remembering the flag. Pass `--author ""` for an intentionally anonymous human post.
- **`ipc/src/channel_manager.rs` + `ipc/src/lifecycle.rs`** — `#atlas` well-known workshop channel. Atlas posts on boot (`atlas <instance> online, seat <role>`) and shutdown (`atlas <instance> offline, seat released`). On boot a filtered SQL scan of `#atlas` (rows beginning with `atlas `, bounded to 5000) decides whether the new Atlas takes Primary or follows.
- **`src/cli.rs::handle_assign_close`** — on-close enforcement. `ryve assign close` refuses a close with zero chat-of-record posts since the assignment's `assigned_at` timestamp, counting posts to either the spark's own channel OR the parent epic's channel (via the `parent_child` bond) so Hands on child tasks are correctly counted.
- **`src/hand_spawn.rs`** — boot-prepend for re-claimed sparks. When `ryve hand spawn` re-spawns on an existing epic, the Hand's initial prompt includes the last N chat-of-record posts from the epic's channel so sudden-death recovery becomes `ryve channel tail` instead of log-file archaeology.
- **`src/mcp/chat.rs`** (via the MCP server) — `chat.post` and `chat.tail` tool wrappers over the CLI primitives, schema-validated, passthrough to the same enforcement paths.

### Channel projection — filtered IRC views (`ryve-06816a07`, PR #53, commit `f07c870`)

Four member sparks:

- **`data/migrations/023_projection_presets.sql`** — new `projection_presets` table. Filter dimensions persisted per workshop + per channel: epic, spark, assignment, PR, actor, optional FTS query. `last_seen_message_id` drives unread badges. Presets survive restart.
- **`ipc/src/channel_projection.rs`** — 5-axis query engine. Any combination of `epic_id` / `spark_id` / `assignment_id` / `pr_number` / `actor_id` ANDs together. FTS5 MATCH on `irc_messages.raw_text` via the migration-024 virtual table. Mention override: messages whose body contains `@<current_actor_id>` bypass the filter axes so `@mentions` always surface. `preset_unread_count` runs `COUNT(*) WHERE id > last_seen_message_id AND <preset filters>` (the arithmetic `max(id) - last_seen` form isn't gap-free so the count form is the correct contract).
- **`src/screen/irc_view.rs`** — new Bench tab kind. Scrollable live message list with sender / timestamp / event-type badge, filter chips per axis, preset picker sidebar with unread badges, FTS search box. Sender label prefers `metadata.actor_id` (event_outbox — human-readable actor tag) falling back to `sender_actor_id` (session FK) so users see `atlas`, `head`, `hand`, `<vendor>` rather than opaque session ids.
- **`src/app.rs`** — wiring. Gear-icon context menu opens an IRC view on the first open epic. `SelectTab` refreshes the IRC view on focus (previously background tabs showed stale content until the next poll tick). Default IRC view channel picks the first `status != "closed"` epic so closed-out epics from prior releases aren't the landing view.

### Merge-Hand contract — precondition-gated + Epic PR lifecycle (`ryve-476ef264`, PR #55, commit `0a24ae2`)

Six member sparks:

- **`src/hand_archetypes.rs`** + **`src/agent_prompts.rs::compose_merge_hand_prompt`** — new `merge_hand` role distinct from the existing `merger`. The prompt pins Assignment-creation order (ascending `assigned_at`, ties broken by spark id), `git merge --no-ff` for every sub-PR merge, forbidden primitives (force-push incl. `--force-with-lease`, `--no-verify`, `[skip ci]` / `[ci skip]` trailers, `gh pr merge --admin`), and Epic PR body template (each child Assignment + source PR, parseable by the E2E test).
- **`src/hand_spawn.rs::check_merge_preconditions`** — precondition checker. Before `spawn_hand(role=merge_hand)` runs it verifies: (a) every child Assignment is in `Approved` phase, (b) no sub-PR has a merge conflict against the Epic branch, (c) CI is green on every sub-PR that has CI, (d) zero Assignments are in `Stuck`. Failures enumerate every offender; the spawn is refused with a structured IRC event on the `#atlas` channel so operators see why the merge didn't start. Typed `CriterionStatus::is_fail()` rather than stringly-typed kind matching.
- **`data/src/sparks/transition.rs::mark_assignment_merged`** — role-locked helper for the `ReadyForMerge → Merged` transition. Only `merge_hand` may drive an Assignment to `Merged` — Head/Director overrides are refused. Stamped by the helper automatically so callers can't forget the actor role.
- **`src/agent_prompts.rs` (conflict handoff prose)** — when a sub-PR fails to merge cleanly, the Merge Hand emits `Approved → Rejected` with `reason=conflict` via the transition validator, which routes ownership back to the originating Hand. The Merge Hand NEVER resolves a sub-PR conflict in-place.
- **`tests/merge_hand_e2e.rs`** — end-to-end path. Seeds two Assignments under an Epic, drives each to `Approved` with a mirrored artifact PR, plays the role of the Merge Hand (creates the Epic branch, integrates both sub-branches with `git merge --no-ff` in Assignment-creation order, composes the Epic PR body, executes Epic → main merge), then drives the merge Assignment through `Approved → ReadyForMerge → Merged` via `mark_assignment_merged`. Asserts every acceptance criterion from the spark.

## Intent behind the implementation

0.2.0 made coordination observable via IRC. 0.2.1 made that observation reliable (bundled IRC daemon, retry, auto-provision). 0.3.0 does two orthogonal things on top of that backbone:

1. **Channels become the memory layer.** Chat-of-record turns the IRC bus from an event stream into a durable record of intent — plans, design picks, blocks, hand-offs. A dead Hand's last thoughts live in `#epic-<id>-<slug>` where the next Hand reads them on boot. Atlas instances that rotate through a workshop inherit `#atlas` history. `ryve assign close` refuses a close with zero posts, so the record isn't optional.

2. **Channels become UI.** Projection renders the same durable record as a filtered, searchable, preset-driven view in the workshop — `show me my Assignments`, `show me @mentions`, `show me PR #42 activity`. Without projection, a busy epic channel is a firehose and users revert to the CLI. With projection, IRC is a first-class UI surface that respects the same event taxonomy as the 0.2.0 signal-discipline filter.

3. **Merges become contract-enforced.** The Merge Hand is the first role in Ryve with a true pre-flight gate. The spawn is refused until every precondition holds, so a Merger can't start on a half-approved epic and surface the failures N minutes later. Conflicts route through the transition validator instead of being resolved in-place, preserving the accountability trail. The role-locked `ReadyForMerge → Merged` edge means the workgraph can't lie about whether an Assignment shipped.

## Known tech debt

1. **Dead-Head-after-dispatch pattern (recurring).** All three 0.3.0 Heads exited silently after dispatching their wave-1 / wave-2 children, claiming a cron poll they never actually created. Manual intervention pushed Hand branches, spawned Mergers, and ran the integration. The build-Head prompt either needs to genuinely create a cron via the proper tool, or block on child closures instead of hallucinating a watcher.

2. **Base-branch stacking lost its race on merge-hand-contract.** `sp-9f6b0c96` (Epic PR lifecycle) was spawned with a declared `blocks` bond against `sp-6b261ad0` (Sub-PR merge flow), but the Hand's worktree didn't base-branch onto `sp-6b261ad0`'s tip — probably because at spawn time the predecessor hadn't pushed its branch yet. Both rewrote `compose_merge_hand_prompt` independently. Resolution was manual. `resolve_hand_base_ref` needs an investigation into the spawn-timing vs. push-timing race.

3. **Merger defaults to `main` for the base branch (recurring from 0.2.1).** Mergers on #53, #54, #55 all opened PRs against `main` instead of `release/0.3.0`. Retargeted by hand each time. Same issue as 0.2.1; still unfixed. The Merger prompt needs the release branch from the release row or parent epic's release bond.

4. **Stable rustfmt ≠ nightly rustfmt (recurring).** `.rustfmt.toml` has unstable features so local `cargo fmt` (nightly) produces a different output than CI's stable `cargo fmt --check`. Tripped PR #55. Worth either locking the fmt toolchain in CI to the same channel, or dropping the unstable-feature directives.

5. **`post_message` writes the DB but never emits on the IRC wire.** The `chat-of-record` contract today is DB-only — other agents read each other's posts via `ryve channel tail`, not via an IRC client subscription. A subscriber on the actual IRC server sees nothing. Documented in the `ipc::chat_of_record` module header; IRC-wire emission is a 0.4.0 candidate.

6. **Initial unread badges on a fresh IRC tab blank for one poll cycle.** The `refresh_irc_view_tab` triple-load batches `load_messages` + `load_presets` + `load_unread_counts` in parallel, but `load_unread_counts` reads `state.presets` which is still empty until `load_presets` returns. Users see badges populate on the next 3 s poll tick. Real fix is Task chaining (`load_presets` → `load_unread_counts`); tracked as a follow-up spark.

7. **`load_unread_counts` is N+1.** One DB roundtrip per preset per poll tick. Acceptable at v1 ceiling (a handful of presets per workshop); worth batching into a grouped query before users accumulate dozens.

8. **`JumpToTail` doesn't actually scroll.** Button updates the model's `scroll_offset_y` but the iced `scrollable` widget owns its own scroll state and isn't bound to a `scrollable::Id`. Banner-clear half works. Needs `scrollable::Id` wiring + `Task` return from the handler (today returns `bool`).

9. **`check_merge_preconditions` is N+1.** One `latest_assignment_for_spark` query per child. Fine at v1 epic sizes (handful → few dozen children); worth a batched `latest_assignments_for_sparks(&ids)` helper before epics grow to 100+.

## How to try out this release

You just need a Ryve workshop running and a terminal. No Rust toolchain needed to exercise these features.

```bash
ryve init                # if this directory isn't already a workshop
ryve                     # open the workshop UI
```

### 1. Chat-of-record — post, read, and enforce

**What you should see:**

- Find an epic's channel name. Run `ryve --json spark show <epic-id>` and look at the title; the canonical channel is `#epic-<id>-<slug>` (where `<slug>` comes from the title). Or: open the workshop UI, click the IRC view tab, and copy the channel from the header.
- Post something: `ryve post --channel '#epic-<id>-<slug>' 'claim: starting on foo bar'`. **Expected:** the command prints a positive message id.
- Read it back: `ryve channel tail --channel '#epic-<id>-<slug>' --limit 10`. **Expected:** your post shows up with a timestamp and (for Hand sessions) the `$RYVE_HAND_SESSION_ID` as the author.
- Inside the workshop, claim a spark under some epic (`ryve assign claim $RYVE_SESSION_ID <spark>`), then immediately try `ryve assign close $RYVE_SESSION_ID <spark>`. **Expected:** the CLI refuses with a message telling you to post a chat-of-record entry first.
- Post to the spark's parent epic's channel, then retry the close. **Expected:** close succeeds. The enforcement works across spark types — a Hand on a task under an epic can post to the epic's channel and it counts.
- **What should NOT happen:** a close with zero posts succeeding; a post to `#atlas` counting for a child spark on an unrelated epic; `tail` returning more than 1000 rows in one call (the limit is capped).

### 2. Sudden-death recovery — re-claimed spark inherits prior context

**What you should see:** (requires a Hand session)

- Spawn a Hand on an unclaimed spark. Let it post a few chat-of-record lines to the epic channel (or post on its behalf via `ryve post`).
- Kill the Hand: find its process via `ryve hand list`, then `kill -9` it. Don't post a handoff.
- After `heartbeat_interval_secs × 2` (default ~60 s) the workshop marks it `at_risk`, then `stuck` after the threshold.
- Recover with `ryve assign override $HEAD_SESSION <spark> --to open --reason "previous Hand died"`.
- Re-spawn a Hand on that spark. **Expected:** the new Hand's boot prompt contains the last N chat-of-record posts from the epic channel, so it can pick up where the prior Hand left off without reading the raw session log.
- **What should NOT happen:** the replacement starting from the spark description with no prior context; the session log file's full contents being dumped into the prompt (that's noise, not record).

### 3. Channel projection — the IRC view tab

**What you should see:**

- Open the workshop UI. Ctrl/Cmd-click the gear icon on the status bar → "Open IRC view" (or the menu entry for it). **Expected:** a new Bench tab opens showing a live message list from the first open epic's channel.
- Filter chips at the top: click "Epic" → pick an epic id → the list narrows. Click "Actor" → pick an actor → narrows again. Multiple chips AND together.
- Type in the search box. **Expected:** FTS5 narrows to messages matching the query (try a word you know is in a recent post).
- Click the "Save as preset" action. Name it `my-work`. Switch away to another tab and back. **Expected:** the preset is still in the sidebar; clicking it restores the filter set.
- Post a new chat-of-record line with `ryve post`. **Expected:** within one 3 s poll tick, the message appears in the live list; if the preset would have matched it, the preset's unread badge increments.
- Send a message containing `@<your-configured-nick>`. **Expected:** the message is surfaced even if the current filter would have excluded it — mention override always breaks through.
- **What should NOT happen:** filters affecting messages addressed to you; the `@ryve` literal bypassing filters for default workshops (mention override is disabled when you haven't explicitly configured a nick); saved presets disappearing after restart; background IRC tabs showing stale content on focus.

### 4. Merge Hand — gated spawn and Epic PR lifecycle

**What you should see:** (requires a Head session + an epic with ≥2 children that have landed sub-PRs)

- Create two `blocks`-bonded sparks under one epic, both touching different files. Spawn Hands, let each open a sub-PR.
- Drive each Assignment through to `Approved` (e.g. get both sub-PRs reviewed + approved on GitHub; the mirror will transition them).
- Have Atlas (or the Head) spawn a Merge Hand: `ryve hand spawn <epic> --role merge_hand --agent claude`. **Expected:** the precondition checker runs first. If every child is `Approved` with no conflicts and CI is green, the spawn succeeds and the Hand starts integrating.
- Watch the Merge Hand's log. **Expected:** it merges each sub-PR into the epic branch with `git merge --no-ff` in ascending `assigned_at` order (ties broken by spark id), then opens or updates the Epic PR against `main` with a body that lists every child Assignment + its source PR. Never `--admin`, never `--force-with-lease`, never `[skip ci]`.
- Force a sub-PR conflict on purpose (land another commit on the epic branch that clashes with a sub-PR). Re-run the spawn path. **Expected:** the Merge Hand does NOT resolve the conflict in-place. Instead it emits `Approved → Rejected` with `reason=conflict` on the offending Assignment, routing ownership back to the originating Hand.
- Approve the Epic PR, make CI green. **Expected:** the Merge Hand merges Epic → main via `gh pr merge --merge --match-head-commit=<sha>` (no `--admin`), then emits the `ReadyForMerge → Merged` transition on its own Assignment via `mark_assignment_merged`.
- **What should NOT happen:** the Merge Hand's spawn succeeding with any child still `InRepair` or `Stuck`; conflicts being resolved in-place; the epic merging into main with `reviewDecision != APPROVED` or any required check not `SUCCESS`; a non-`merge_hand` actor driving an Assignment to `Merged` (the transition validator refuses it).

### 5. End-to-end sanity check

Post a chat-of-record line from the CLI, read it back from `ryve channel tail`, see it in the workshop's IRC view tab with the correct sender label, save a filter preset, kill a Hand mid-task, confirm the replacement inherits the dead Hand's posts. If you can do that loop with IRC pills green throughout, 0.3.0 is working as intended.

### For maintainers

The automated test suite backing the above is in `cargo test --workspace` (requires the Rust toolchain). New in 0.3.0: `ipc/tests/chat_of_record.rs` (12 tests covering post / tail / count gating with parent-epic fallback), `ipc/tests/channel_projection.rs` (15 tests covering the 5 axes + FTS + mention override + preset unread), `tests/merge_hand_e2e.rs` (full happy-path integration). Twelve `hand_spawn` tokio tests from 0.2.0 remain `#[ignore]` on CI (run locally with `cargo test --workspace -- --ignored --test-threads=1`).
