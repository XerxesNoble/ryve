# Release 0.2.1 — Resilience + Bundled Coordination + Visibility

**Release ID:** `rel-90955b83`
**Branch:** `release/0.2.1`
**Date:** 2026-04-19
**Main merge commit:** `12e38fb4`

## What was implemented

Where 0.2.0 built the coordination surface (IRC bus + GitHub mirror + heartbeat), 0.2.1 makes that surface *reliable and visible*. Four epics land together: Hand sessions no longer die silently under load, serialized children build on each other's work instead of colliding at merge time, IRC works out of the box without a user-provided server, and every 0.2.0 subsystem now surfaces in the workshop UI as a first-class indicator.

### D — Heavy-test stream-timeout fix (`ryve-32e95c28`, PR #44, commit `8fbbc34`)

Six member sparks:

- **`src/stream_heartbeat.rs`** — a new `StreamHeartbeat` tokio wrapper that spawns a child with piped stdout/stderr and injects a heartbeat line onto the caller's sink whenever the child has been silent for longer than a configured interval (default 30 s). Preserves per-stream byte order; `Command::kill_on_drop(true)` + explicit cleanup on error paths so a broken sink never leaks a long-running `cargo test`.
- **`src/cli.rs` → `ryve hand exec-heartbeat`** — stateless CLI that drives the wrapper from a Hand's shell. Dispatched before workshop discovery so it works in any cwd (including a fresh CI checkout with no `.ryve/`).
- **`src/hand_spawn.rs::run_with_stream_heartbeat`** — integration point that the spawn path uses for Hand subprocesses known to stay silent for minutes (full-workspace `cargo test`, cold `cargo build --release`, large `sqlx migrate`).
- **`tests/heavy_test_stream_survival.rs`** — end-to-end test that spawns `ryve hand exec-heartbeat` against a 4 s silent child with a 1 s interval, drains stdout on a background thread that timestamps each line, and asserts at least one heartbeat arrives before `T0 + (child_silent_secs − 1)`. A wrapper that buffered heartbeats and flushed at child exit would fail the check.
- **`docs/STREAMING_HEARTBEAT.md`** — written pattern for when new heavy subcommands must adopt the wrapper (threshold: 60 s 95th-percentile runtime or 60 s silent on plausible input).

### A — Base-branch stacking for serial children (`ryve-b7633430`, PR #47, commit `40ea8b1e`)

Three member sparks:

- **`data/src/sparks/bond_repo.rs::list_blocks_predecessors`** — helper that returns every `blocks` predecessor for a spark id, ordered by bond creation.
- **`src/workshop.rs::create_hand_worktree`** — now takes an optional `base_ref: Option<&str>`; when `Some`, the worktree is cut from that ref instead of the workshop's current HEAD.
- **`src/hand_spawn.rs::resolve_hand_base_ref`** — at spawn time, queries `bond_repo::list_blocks_predecessors` for the new Hand's spark, walks every owner assignment on each predecessor (newest-first), tries each candidate `<actor>/<short>` branch via `git rev-parse`, and returns the first tip SHA that resolves. A Hand on a spark with no `blocks` predecessors still cuts from the release base (unchanged behaviour).
- **`tests/hand_base_branch_stacking.rs`** — regression test that seeds two sparks with a `blocks` bond, both editing the same sentinel line, and asserts the Merger integrates them with zero merge conflicts.

### B — Bundled IRC server, default-on (`ryve-31659bbb`, PRs #46 + #48, commits `88af5537` + `c884b72f`)

Six member sparks:

- **`build.rs` + `scripts/build-vendored-ircd.sh` + `vendor/ircd/VERSION`** — ngIRCd is vendored from upstream and built alongside Ryve by `build.rs`. Stamp-file semantics mirror `VENDORED_TMUX`: the build re-runs on version change, skips on identical stamps, is opt-outable via env var.
- **`src/bundled_ircd.rs`** — runtime resolver. Production calls `bundled_ircd_path()` which threads in the real `std::env::current_exe()` → `<exe_dir>/bin/ngircd` (installed layout) or `<repo_root>/vendor/ircd/bin/ngircd` (dev layout). Tests drive a `resolve_bundled_ircd_path_from(exe_dir)` helper against a TempDir so the resolver order is actually exercised.
- **`src/ircd_process.rs`** — `IrcdSupervisor` with reconcile-then-spawn semantics: pidfile check → port probe → fresh spawn. SIGTERM-then-SIGKILL graceful shutdown. `Command::kill_on_drop(true)` + explicit kill on post-spawn pidfile-write failure so a fallible init path cannot leak a daemon.
- **`data/src/ryve_dir.rs::WorkshopConfig`** — new `irc_enabled: bool` (default `true` on fresh workshops; explicit opt-out preserved on re-init), `irc_bundled_port: Option<u16>`, `effective_irc_server_address()` helper that resolves `irc_server` → `127.0.0.1:<irc_bundled_port>` → `None`.
- **`src/cli.rs::handle_init` → `provision_workshop_ircd`** — `ryve init` allocates a free loopback port, writes `.ryve/ircd/ircd.conf`, and records the port + `irc_enabled = true` in the workshop config. Re-running is idempotent and preserves a user's explicit opt-out.
- **`src/app.rs`** — workshop open wires the supervisor (installs into a race-safe `Arc<Mutex<Option<_>>>` that detects a concurrent close via `Arc::strong_count` and drives shutdown directly rather than leaving a widow supervisor), and close tears it down after the IRC runtime disconnects so the client's QUIT reaches the daemon before we SIGTERM it.
- **`tests/bundled_ircd_e2e.rs`** — fresh `ryve init` in a TempDir → IPC runtime starts supervised daemon → IRC client connects to `127.0.0.1:<bundled_port>` and joins a channel. No outbound network, no user configuration.

### C — UI surfacing (`ryve-8918fd04`, PR #49, commit `32e92cac`)

Three member sparks:

- **`src/screen/status_bar.rs`** — new `IrcStatus` (`Connected` / `Disconnected` / `Disabled`) and `GitHubStatus` (`Configured` / `Unconfigured` / `Error`) pills. `GitHubStatus::from_config` takes `is_configured: bool` from the full `GitHubConfig::is_configured()` so the newer Settings flow using `webhook_secret` + `poll_token` is reflected correctly; the legacy `token` + `repo` pair is still inspected so a half-configured legacy setup surfaces as `Error`.
- **`src/screen/sparks.rs`** — epic cards now render a liveness badge (`Healthy` / `At Risk` / `Stuck` / `Unknown` — label and colour agree on the fallback), an IRC channel pill (`#epic-<id>-<slug>`, shown in accent when the runtime is connected, hidden otherwise so users don't mistake a muted pill for an active-but-idle channel), and a GitHub artifact PR link once the applier has recorded one.
- **`src/screen/settings.rs`** + **`src/screen/integrations.rs`** — new modal overlays. The status-bar gear icon emits `OpenIntegrations` (renamed from `OpenSettings` to match where it actually routes), which opens the Integrations health view (live IRC server/port/nick/known channels + GitHub repo/mode/configured). From Integrations, an "Edit" button opens the Settings credentials form, where the user sets `irc_server` and the GitHub credentials (`webhook_secret`, `poll_token`) without hand-editing `.ryve/config.toml`. Settings commit persists the config; in-flight save tasks now own a `WorkshopConfig` snapshot instead of an `Arc`, so concurrent edits cannot race through stale writes.

## Intent behind the implementation

0.2.0 made a group of agents observable; 0.2.1 makes that observation *reliable and reachable*. Three properties:

1. **Long-running work doesn't die silently.** The stream-heartbeat wrapper keeps the parent transport alive during any silent child, so Claude's ~5-minute stream-idle timeout no longer kills a Hand mid-`cargo test`. The pattern is codified in `docs/STREAMING_HEARTBEAT.md` with a reviewer checklist so future heavy subcommands adopt it by default.

2. **Serialized children stop stepping on each other.** Base-branch stacking moves what used to be an N-way merge-time integration to a 1-way dispatch-time stack. The 0.2.0 retro flagged three migration-019 collisions; the 0.2.1 regression test shows two children editing the same sentinel line now integrate with zero conflicts.

3. **Coordination primitives work out of the box and are visible in the UI.** Ryve no longer asks "what IRC server should I use?" — it ships one, provisions it on `ryve init`, supervises it across launches, and preserves your opt-out when you don't want it. And every one of the 0.2.0 subsystems (IRC connection, GitHub integration, heartbeat liveness) is now a live indicator in the workshop UI rather than a config-file secret.

Together, D turns "Hand dies during heavy test" from a silent timeout into a non-event; A turns "same-file collisions at merge time" from a recurring hazard into a test-covered no-op; B turns "user must bring their own IRC server" into an install-and-go default; C turns "look in `.ryve/config.toml` to see subsystem state" into real-time pills and forms in the workshop.

## Known tech debt

1. **Lefthook pre-push deadlock (recurring).** Merger PRs on PR #46, #48, and #49 all hit `Connection to github.com closed by remote host` during `lefthook pre-push` (which runs `cargo clippy` + `cargo fmt --check` on a cold target). Each time Atlas manually pushed with `--no-verify` once local clippy came back clean. Structural fix: either skip the hook on integration branches (the same gates run in CI) or move the hook's heavy work off the push path.

2. **Chicken-and-egg during Epic A dispatch.** The Hand that implements base-branch stacking (`ryve-6ef81933`) needs base-branch stacking to be dispatched correctly, which it wasn't because Epic A hadn't landed yet. Atlas worked around it by manually merging the two predecessor branches into the Hand's worktree before the Hand started. Future releases that build foundation primitives should stage them behind a trivial bootstrap commit so the test spark can dispatch against its own feature already landing.

3. **Disk-full during heavy parallel builds.** Three Epic C Hands building 1M-LOC dependency graphs in parallel exhausted 1.8 TB on a laptop because every Hand worktree had its own `target/` (each ~8 GB after first build). One Hand session died during the ENOSPC event with ~1200 LOC of uncommitted work; Atlas rescued it by committing the worktree contents manually. Structural fix options: shared `target/` across worktrees via `CARGO_TARGET_DIR`, aggressive `cargo clean` on spark close, or a GC sweep that prunes `target/` dirs on any worktree whose spark is closed.

4. **Duplicate Hand auto-spawn during disk-full recovery.** While Atlas was freeing disk, Ryve's stuck-detection re-spawned a second Hand on `ryve-c3de335e` (session `3e8cac25`) in parallel with the original session `3e5fe109` that was actually fine. Both ended up rewriting Settings + Integrations independently. Atlas accepted `3e5fe109`'s work (committed first) and released `3e8cac25`'s claim. The re-spawn threshold should probably respect a recent-write window on the Hand's log before assuming the Hand is stuck.

5. **Merger opens PR against `main` instead of the release branch (recurring).** Every Merger this release (PRs #46, #47, #49) opened against `main` by default. Atlas retargeted each one manually. The Merger prompt needs to learn about the release branch from the release row (or the parent epic's release bond) rather than always defaulting to `main`.

6. **Stale crew branch on origin after force-recycle.** PR #46's `crew/cr-a6d6c4ee` stayed on origin after the PR merged + local branch deleted. When Atlas spawned a second Merger on the same crew for PR #48's children, it had to explicitly `git push --delete` the stale remote branch first, or the new Merger would force-push over a merged commit. Either the squash-merge cleanup should delete the remote branch reliably, or subsequent Mergers should use a `-v2` suffix by convention (as PR #48 did).

7. **`GitHubStatus` still accepts the legacy token+repo pair.** PR #49 Copilot c4 pushed the full-config flag into `from_config`, but the legacy pair is still inspected to report partial-setup as `Error`. A future release should remove the legacy pair entirely once there are no callers that still set `github.token` + `github.repo` directly.

## How to try out this release

You just need a Ryve workshop running and a terminal. No Rust toolchain needed to exercise these features.

```bash
ryve init                # on a fresh directory
ryve                     # open the workshop UI
```

### 1. Bundled IRC — your workshop is already on-mesh

**What you should see:**

- Immediately after `ryve init`, check `.ryve/` in your workshop directory. **Expected:** you see `.ryve/ircd/ircd.conf` with a workshop-scoped port (a line like `Ports = 45123` or similar) and `Listen = 127.0.0.1` (loopback only; this daemon is for you alone).
- Check `.ryve/config.toml`. **Expected:** `irc_enabled = true` is recorded explicitly, and `irc_bundled_port = <same port>` is set.
- Launch Ryve (`ryve`). Look at the status bar (bottom of the window). **Expected:** the IRC pill shows "Connected" within a couple of seconds. That means Ryve has spawned the bundled daemon, your client connected to it, and the two are running on loopback.
- Close Ryve. Relaunch. **Expected:** the pill flips straight to "Connected" with no ngIRCd process-start delay, because the supervisor reconciled to the already-running daemon from the previous session.
- Run `ryve init` a second time in the same workshop. **Expected:** the port in `.ryve/ircd/ircd.conf` and `.ryve/config.toml` does not change. (Idempotent.)
- To opt out, edit `.ryve/config.toml` and set `irc_enabled = false`. Re-run `ryve init`. **Expected:** the flag stays `false` across subsequent inits — Ryve never silently re-enables IRC for you.
- **What should NOT happen:** Ryve prompting for an IRC server URL; IRC refusing to start because no server is configured; an `irc_enabled = false` setting flipping back to `true` on init.

### 2. UI surfacing — every subsystem is a live pill

**What you should see:**

- Open Ryve on a workshop that has epics + sparks. Look at the status bar. **Expected:** three pills — IRC (Connected / Disconnected / Disabled), GitHub (Configured / Unconfigured / Error), and the usual branch + file + active-Hand counters.
- Click the gear icon at the right of the status bar. **Expected:** the Integrations overlay opens showing IRC's live state (server/port/nick/known channel count) and GitHub's (repo/mode/auto-sync/configured).
- In Integrations, click "Edit". **Expected:** the Settings form opens. Fill in `irc_server` (e.g. `irc.libera.chat:6697`) or GitHub credentials (`webhook_secret` + `poll_token`). Hit Save.
- Check `.ryve/config.toml`. **Expected:** your edits are persisted; the pills update within a tick.
- Open an epic card in the Sparks screen. **Expected:** a liveness badge (Healthy / At Risk / Stuck / Unknown) renders on any epic whose Head has an active assignment, an IRC channel pill (`#epic-<id>-<slug>`) renders when IRC is connected, and a PR link renders once the GitHub applier has seen an artifact for any child spark.
- Deliberately make a Hand stuck (from 0.2.0 — `kill -9` its process). **Expected:** the liveness badge flips from Healthy → At Risk → Stuck within the heartbeat intervals, **and the colour of the badge now matches the label** (the Unknown state is muted grey with the "Unknown" label, not "Healthy" in disabled grey).
- **What should NOT happen:** pills stuck at "Unconfigured" after you've filled in Settings; liveness badges saying "Healthy" in grey (the 0.2.1 fix for that exact mismatch); Settings edits only applying after a workshop restart.

### 3. Base-branch stacking — serial children no longer collide

**What you should see:** (requires a Head session)

- Create two sparks serialized via a `blocks` bond, both declaring the same file in their scope: e.g. `ryve spark create --type task --scope "src/foo.rs" "Rename symbol A"` then `ryve spark create --type task --scope "src/foo.rs" "Rename symbol B" --blocks <first-id>`.
- Spawn Hands on both sparks in order.
- **Expected:** the second Hand's worktree (`.ryve/worktrees/<short>/`) contains the first Hand's commit as an ancestor. `git log --oneline HEAD` in the second worktree shows the first Hand's commit before the base.
- Let both Hands finish. Kick off a Merger on the parent epic.
- **Expected:** the Merger integrates both Hand branches with **zero merge conflicts**. In 0.2.0 this scenario reliably produced an unresolvable same-hunk conflict (the migration-019 collision pattern).
- **What should NOT happen:** the Merger asking you to resolve a same-hunk conflict; the second Hand's worktree starting from the release base without seeing the first Hand's work.

### 4. Stream-heartbeat wrapper — long `cargo test` no longer kills Hands

**What you should see:** (requires a Hand session)

- Spawn a Hand and give it a task that will run a full-workspace `cargo test`.
- Watch the Hand's log (`.ryve/logs/hand-<session>.log`). **Expected:** every 30 s of silent child output, a line appears: `[stream-heartbeat] still running`. The child's real stdout/stderr (cargo's "Compiling foo", "running N tests", etc.) passes through unchanged when the child is noisy.
- Let the `cargo test` run to completion — including the >5 minute quiet stretch where cargo is linking rlibs with nothing to print. **Expected:** the Hand session stays alive past the 5-minute window; before 0.2.1 it would have died with `Stream idle timeout - partial response received` and the Hand's work would have been lost.
- **What should NOT happen:** a Hand silently dying mid-test; heartbeat lines appearing while the child is actively printing (they only fire during silence); heartbeats appearing after the child exits.

### 5. End-to-end sanity check

Run `ryve init` in a fresh directory, launch `ryve`, confirm the IRC pill goes green, open Settings from the gear icon, fill in a GitHub token, confirm the GH pill goes green, spawn a Hand on a test spark, watch its liveness badge stay Healthy while it works, and watch a `#epic-...` channel appear on the bundled IRC daemon. If you can do that loop without editing `.ryve/config.toml` by hand, 0.2.1 is working as intended.

### For maintainers

The automated test suite backing the above is in `cargo test --workspace` (requires the Rust toolchain). Most flow is covered by library + integration tests; twelve `hand_spawn` tokio tests from 0.2.0 remain `#[ignore]` on CI (run locally with `cargo test --workspace -- --ignored --test-threads=1`). The Hand spawn path itself now uses `ryve hand exec-heartbeat` for subprocesses known to stay silent for minutes — see `docs/STREAMING_HEARTBEAT.md` for when to adopt the wrapper on new heavy subcommands.
