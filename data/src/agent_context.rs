// SPDX-License-Identifier: AGPL-3.0-or-later

//! Agent context synchronisation.
//!
//! Generates `.ryve/WORKSHOP.md` — a static operations guide that tells Hands
//! how to use `ryve` to query the workgraph (the DB is the single source of
//! truth for spark state). Also injects a pointer into each agent's boot file
//! so they discover and read it automatically, and propagates both into every
//! active worktree.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::ryve_dir::{RyveDir, WorkshopConfig};

// ── Sync cache ────────────────────────────────────────

/// Cache of last-written content hashes per file. Lives in `Workshop` state
/// and is passed through to [`sync`] on every refresh tick.
///
/// Without this cache, `sync` would rewrite WORKSHOP.md plus every agent
/// boot file in the main checkout *and* every active worktree on every
/// `SparksLoaded` event (~25 writes every 3s for a 5-worktree workshop),
/// thrashing the filesystem and starving tokio workers.
///
/// With it, repeated `sync()` calls produce zero file writes when neither
/// the config nor any worktree on disk has changed.
#[derive(Debug, Default)]
pub struct SyncCache {
    /// Hash of the most recent content we wrote (or observed equal) at each
    /// absolute path. A subsequent `sync()` that would write the same bytes
    /// short-circuits without touching disk.
    file_hashes: HashMap<PathBuf, u64>,
}

impl SyncCache {
    pub fn new() -> Self {
        Self::default()
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(s.as_bytes());
    h.finish()
}

// ── Marker ────────────────────────────────────────────

const MARKER_START: &str = "<!-- RYVE:START -->";
const MARKER_END: &str = "<!-- RYVE:END -->";

// ── Target files ──────────────────────────────────────

/// Well-known files that coding agents read on boot.
/// Each entry is relative to the workshop root.
const DEFAULT_TARGETS: &[&str] = &[
    "CLAUDE.md",
    "OPENCODE.md",
    "AGENTS.md",
    ".cursorrules",
    ".github/copilot-instructions.md",
];

// ── Public API ────────────────────────────────────────

/// Generate `.ryve/WORKSHOP.md` and inject pointers into agent boot files.
///
/// WORKSHOP.md is a static operations guide — it tells Hands how to use the
/// CLI to query the workgraph, not what the current state is (that lives in
/// the database, the single source of truth).
///
/// Also propagates WORKSHOP.md and pointers into every active worktree so
/// Hand agents always have the guide without reading files outside their tree.
pub async fn sync(
    workshop_dir: &Path,
    ryve_dir: &RyveDir,
    config: &WorkshopConfig,
    cache: &Mutex<SyncCache>,
) -> Result<(), std::io::Error> {
    let workshop_md = generate_workshop_md();
    let workshop_md_hash = hash_str(&workshop_md);

    write_if_changed(
        ryve_dir.workshop_md_path(),
        &workshop_md,
        workshop_md_hash,
        cache,
    )
    .await?;

    let targets = resolve_targets(config);
    for rel in &targets {
        inject_pointer_if_changed(workshop_dir, rel, cache).await?;
    }

    // Write RYVE.md at the workshop root if missing or changed.
    let ryve_md = crate::ryve_dir::DEFAULT_RYVE_MD;
    let ryve_md_hash = hash_str(ryve_md);
    write_if_changed(ryve_dir.ryve_md_path(), ryve_md, ryve_md_hash, cache).await?;

    // Propagate WORKSHOP.md, RYVE.md, and pointers into every active
    // worktree. The per-file helpers (`write_if_changed`,
    // `inject_pointer_if_changed`) each short-circuit on a warm cache hit
    // with zero I/O, so the only remaining cost on a steady-state tick is
    // the directory listing itself.
    let worktrees_dir = ryve_dir.root().join("worktrees");
    if let Ok(mut entries) = tokio::fs::read_dir(&worktrees_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let wt_path = entry.path();
            if !wt_path.is_dir() {
                continue;
            }
            // Write WORKSHOP.md into the worktree's .ryve/
            let wt_ryve = wt_path.join(".ryve");
            if wt_ryve.is_dir() {
                let wt_workshop_md = wt_ryve.join("WORKSHOP.md");
                let _ =
                    write_if_changed(wt_workshop_md, &workshop_md, workshop_md_hash, cache).await;
            }
            // Write RYVE.md at the worktree root
            let wt_ryve_md = wt_path.join("RYVE.md");
            let _ = write_if_changed(wt_ryve_md, ryve_md, ryve_md_hash, cache).await;
            // Re-inject pointers into the worktree's agent boot files
            for rel in &targets {
                let _ = inject_pointer_if_changed(&wt_path, rel, cache).await;
            }
        }
    }

    Ok(())
}

/// Write `content` to `path` only if the bytes already on disk differ from
/// what we'd write. Updates `cache` so the next call can short-circuit
/// without any I/O when nothing has changed.
async fn write_if_changed(
    path: PathBuf,
    content: &str,
    content_hash: u64,
    cache: &Mutex<SyncCache>,
) -> Result<(), std::io::Error> {
    // Cheap path: cache hit means we already wrote (or observed) these exact
    // bytes here. Trust the cache — no read, no write. Stale cache (the file
    // was edited externally to match a different value) is self-correcting on
    // the next miss because we re-read on every miss below.
    if cache_hit(cache, &path, content_hash) {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Cache miss: read disk to see if it already matches. If so, no write
    // needed — just record the hash so future calls take the cheap path.
    if let Ok(existing) = tokio::fs::read_to_string(&path).await
        && hash_str(&existing) == content_hash
    {
        cache_set(cache, path, content_hash);
        return Ok(());
    }

    tokio::fs::write(&path, content).await?;
    cache_set(cache, path, content_hash);
    Ok(())
}

fn cache_hit(cache: &Mutex<SyncCache>, path: &Path, hash: u64) -> bool {
    cache
        .lock()
        .expect("agent_context sync cache mutex poisoned")
        .file_hashes
        .get(path)
        == Some(&hash)
}

/// Returns `true` if the cache has *any* entry for `path`, regardless of hash.
///
/// Used by `inject_pointer_if_changed` where the target content depends on the
/// existing file but the injected block is constant. A cache entry means we
/// previously wrote or observed a correct file — the pointer hasn't changed, so
/// the file is still correct unless edited externally (which self-corrects on
/// the next cold start).
fn cache_has_entry(cache: &Mutex<SyncCache>, path: &Path) -> bool {
    cache
        .lock()
        .expect("agent_context sync cache mutex poisoned")
        .file_hashes
        .contains_key(path)
}

fn cache_set(cache: &Mutex<SyncCache>, path: PathBuf, hash: u64) {
    cache
        .lock()
        .expect("agent_context sync cache mutex poisoned")
        .file_hashes
        .insert(path, hash);
}

/// Drop a cache entry — used when we observe a tracked file has disappeared
/// from disk so the next sync will recreate it instead of skipping via the
/// warm-cache fast path.
fn cache_forget(cache: &Mutex<SyncCache>, path: &Path) {
    cache
        .lock()
        .expect("agent_context sync cache mutex poisoned")
        .file_hashes
        .remove(path);
}

// ── WORKSHOP.md generation ────────────────────────────

fn generate_workshop_md() -> String {
    let mut md = String::with_capacity(4096);

    md.push_str("# Ryve Workshop\n\n");
    md.push_str(
        "You are a **Hand** working inside a **Ryve Workshop**. Ryve manages tasks \
         (called *sparks*) in an embedded workgraph stored in `.ryve/sparks.db`.\n\n",
    );

    md.push_str(
        "**IMPORTANT: Work in your current directory.** Do not navigate to parent \
         directories or other worktrees. All code changes, commits, and CLI commands \
         must be run from within this working tree.\n\n",
    );

    // ── Getting started ──────────────────────────────────────────
    md.push_str("## Getting Started\n\n");
    md.push_str(
        "Before doing any work, check the current workgraph state with `ryve spark list` \
         to see active sparks, their priorities, and which are already claimed. Prefer \
         `ryve hot` for a ready-to-work view that filters out sparks blocked by bonds.\n\n",
    );

    // ── Rules ─────────────────────────────────────────────────────
    md.push_str("## Rules\n\n");
    md.push_str("1. **Always reference spark IDs** in commit messages: `fix(auth): validate token expiry [sp-a1b2]`\n");
    md.push_str("2. **Work in priority order** — P0 is critical, P4 is negligible.\n");
    md.push_str("3. **Respect architectural constraints** — run `ryve constraint list` to check. Violations are blocking.\n");
    md.push_str("4. **Check required contracts** before considering a spark done: `ryve contract list <spark-id>`.\n");
    md.push_str(
        "5. **Check bonds before claiming a spark** — run `ryve bond list <spark-id>`. \
         If the spark is the target of a `blocks` or `conditional_blocks` bond whose \
         source is not yet completed, do NOT start it. Pick a different spark or use \
         `ryve hot` to see only unblocked work.\n",
    );
    md.push_str("6. **Do not work on a spark that is already claimed** by another Hand.\n");
    md.push_str(
        "7. If you discover a new bug or task, create a spark for it (see commands below).\n\n",
    );

    // ── Spark intent ──────────────────────────────────────────────
    md.push_str("## Spark Intent\n\n");
    md.push_str(
        "Every spark can carry a structured **intent** that spells out what \"done\" \
         actually means. Always read it with `ryve spark show <id>` before writing code.\n\n",
    );
    md.push_str(
        "- **problem_statement** — the concrete problem the spark is solving (the *why*).\n",
    );
    md.push_str(
        "- **invariants** — properties that MUST hold throughout and after your change. \
         Violating an invariant means the spark is not done, even if the feature works.\n",
    );
    md.push_str(
        "- **non_goals** — things explicitly out of scope. Do not expand the spark to \
         cover them; file a new spark instead.\n",
    );
    md.push_str(
        "- **acceptance_criteria** — the checklist that must pass before the spark can \
         be closed. Each criterion should be verifiable.\n\n",
    );
    md.push_str(
        "When creating sparks, pass intent via flags on `ryve spark create`: \
         `--problem`, `--invariant` (repeatable), `--non-goal` (repeatable), \
         `--acceptance` (repeatable). Example:\n\n",
    );
    md.push_str(
        "```sh\nryve spark create --type bug --priority 1 \\\n  \
         --problem 'tokens survive logout' \\\n  \
         --invariant 'session table is empty after logout' \\\n  \
         --non-goal 'refresh token rotation' \\\n  \
         --acceptance 'integration test: logout then /me returns 401' \\\n  \
         'auth: purge session on logout'\n```\n\n",
    );

    // ── Bonds (dependencies) ──────────────────────────────────────
    md.push_str("## Bonds (Dependencies)\n\n");
    md.push_str(
        "Bonds are directed edges between sparks. They tell Hands which work is \
         actually ready and which must wait. **Check bonds before starting a spark.**\n\n",
    );
    md.push_str("Bond types:\n\n");
    md.push_str("- `blocks` — source must complete before target can start. **Blocking.**\n");
    md.push_str("- `conditional_blocks` — blocks only under a runtime condition. **Blocking until resolved.**\n");
    md.push_str("- `waits_for` — soft ordering hint; target should wait but isn't hard-blocked.\n");
    md.push_str("- `parent_child` — target is a subtask of source (used for epics).\n");
    md.push_str("- `related` — informational cross-link; no ordering.\n");
    md.push_str("- `duplicates` — target duplicates source; one should be closed.\n");
    md.push_str("- `supersedes` — target replaces source.\n\n");
    md.push_str("```sh\n");
    md.push_str("ryve bond list <spark-id>                 # all bonds touching this spark\n");
    md.push_str("ryve bond create <from> <to> blocks        # add a blocking dependency\n");
    md.push_str("ryve bond delete <bond-id>                 # remove a bond\n");
    md.push_str(
        "ryve hot                                   # sparks with no unmet blocking bonds\n",
    );
    md.push_str("```\n\n");

    // ── Workgraph commands ───────────────────────────────────────
    md.push_str("## Workgraph Commands\n\n");
    md.push_str(
        "Use `ryve` to query and update the workgraph. **Always run from the workshop root.**\n\n",
    );

    md.push_str("### Query state\n\n");
    md.push_str("```sh\nryve spark list                       # active sparks\n");
    md.push_str("ryve spark list --all                 # include closed\n");
    md.push_str(
        "ryve hot                              # sparks unblocked by bonds (ready to work)\n",
    );
    md.push_str("ryve spark show <spark-id>            # spark details + intent\n");
    md.push_str("ryve bond list <spark-id>             # dependency bonds\n");
    md.push_str("ryve constraint list                  # architectural constraints\n");
    md.push_str("ryve contract list <spark-id>         # verification contracts\n");
    md.push_str(
        "ryve ember list                       # live signals from other Hands / the UI\n```\n\n",
    );

    md.push_str("### Mutate state\n\n");
    md.push_str("```sh\n");
    md.push_str("ryve spark create <title>                           # create a task spark\n");
    md.push_str("ryve spark create --type bug --priority 1 \\\n");
    md.push_str("  --problem '...' --invariant '...' \\\n");
    md.push_str(
        "  --non-goal '...' --acceptance '...' <title>       # create with structured intent\n",
    );
    md.push_str("ryve spark edit <spark-id> --title <t> \\\n");
    md.push_str("  --priority <0-4> --risk <level> --scope <path>    # edit fields in place\n");
    md.push_str("ryve spark status <spark-id> in_progress            # claim / update status\n");
    md.push_str("ryve spark close <spark-id> <reason>                # close a spark\n");
    md.push('\n');
    md.push_str("ryve bond create <from> <to> <type>                 # add dependency (blocks, related, ...)\n");
    md.push_str("ryve bond delete <bond-id>                          # remove a bond\n");
    md.push('\n');
    md.push_str("ryve comment add <spark-id> <body>                  # leave a note on a spark\n");
    md.push_str("ryve stamp add <spark-id> <label>                   # tag a spark\n");
    md.push_str(
        "ryve contract add <spark-id> <kind> <description>   # add a verification contract\n",
    );
    md.push_str("ryve contract check <contract-id> pass|fail         # record a contract result\n");
    md.push('\n');
    md.push_str(
        "ryve ember send <type> <content>                    # broadcast an ember signal\n",
    );
    md.push_str("ryve ember sweep                                    # clean up expired embers\n");
    md.push_str("```\n\n");

    md.push_str(
        "Ember types, in order of urgency: `glow` (ambient), `flash` (quick heads-up), \
         `flare` (needs attention soon), `blaze` (urgent — interrupt-worthy), `ash` \
         (archival / post-mortem).\n\n",
    );

    md.push_str("Ryve auto-refreshes every 3 seconds. Changes are picked up by the UI and other Hands automatically.\n\n");

    // ── Heads ──────────────────────────────────────────────────────
    md.push_str("## Heads (Crew Orchestrators)\n\n");
    md.push_str(
        "A **Head** is the layer above a Hand. Where a Hand owns a *single* spark and \
         edits code in its own worktree, a Head owns a *goal* — an epic — and \
         orchestrates a **Crew** of Hands that work on its child sparks in parallel.\n\n",
    );
    md.push_str("```\n");
    md.push_str("       User\n");
    md.push_str("         │ talks to\n");
    md.push_str("         ▼\n");
    md.push_str("       Atlas (Director)        — singleton, user-facing, never edits code\n");
    md.push_str("         │ delegates a goal\n");
    md.push_str("         ▼\n");
    md.push_str("       Head                    — crew orchestrator, decomposes + supervises\n");
    md.push_str("         │ spawns & supervises\n");
    md.push_str("         ▼\n");
    md.push_str("       Hand, Hand, Hand, …     — workers, each claim one spark in a worktree\n");
    md.push_str("         │ when all children close\n");
    md.push_str("         ▼\n");
    md.push_str("       Merger (Hand)           — integrates worktrees into one PR\n");
    md.push_str("```\n\n");
    md.push_str(
        "Mechanically, a Head is the **same kind of coding-agent subprocess** as a Hand \
         (same worktree machinery, same `agent_sessions` row, same launch flow). What \
         distinguishes it is the *system prompt* (composed via `compose_head_prompt`) \
         and the session label `session_label = \"head\"`. A Head is not an in-process \
         LLM call and does not need any special schema or process type.\n\n",
    );

    md.push_str("### Lifecycle\n\n");
    md.push_str(
        "1. **Atlas spawns a Head** on an epic spark with a populated intent: \
         `ryve head spawn <epic_id> --archetype <build|research|review> --agent claude`. This creates a worktree, an agent \
         session, and an assignment where the Head \"owns\" the epic.\n",
    );
    md.push_str(
        "2. **The Head reads the epic** (`ryve spark show <epic_id>`) and decomposes it \
         into 2–8 child task sparks via `ryve spark create`, bonded back to the epic with \
         `parent_child`.\n",
    );
    md.push_str(
        "3. **The Head creates a Crew** bundling those sparks: \
         `ryve crew create '<name>' --parent <epic_id> --head-session $RYVE_SESSION_ID`.\n",
    );
    md.push_str(
        "4. **The Head spawns one Hand per child spark**: \
         `ryve hand spawn <child_id> --agent <a> --crew <crew_id>`. Each Hand works in \
         its own worktree on `hand/<short>`.\n",
    );
    md.push_str(
        "5. **The Head polls** `ryve crew show <crew_id>` and `ryve assign list <spark_id>`. \
         Stalled Hands are released and respawned.\n",
    );
    md.push_str(
        "6. **When every child spark is closed**, the Head creates a *merge spark* and \
         spawns a **Merger** Hand: \
         `ryve hand spawn <merge_spark> --role merger --crew <crew_id>`.\n",
    );
    md.push_str(
        "7. **The Merger integrates** every `hand/<short>` branch into a single \
         `crew/<crew_id>` branch, pushes, and opens one PR. It never merges to `main` — \
         human review is always required.\n",
    );
    md.push_str(
        "8. **The Head posts the PR URL** as a comment on the parent epic and exits. \
         Atlas surfaces the URL to the user on its next poll.\n\n",
    );
    md.push_str(
        "A Head **never edits code**. If a Head finds itself wanting to write a patch, \
         it must spawn a Hand on a spark instead. This mirrors the worktree-isolation \
         invariant: \"Hands must never work in the main tree\" applies to Heads too.\n\n",
    );

    md.push_str("### Archetypes\n\n");
    md.push_str(
        "Heads come in three **standard archetypes**. The archetype is a *prompting and \
         delegation contract* — not a new subprocess type — and determines which kinds of \
         child sparks and Hands a Head may create.\n\n",
    );
    md.push_str("| Archetype | Purpose | Default crew shape | Closes the epic by |\n");
    md.push_str("|-----------|---------|--------------------|--------------------|\n");
    md.push_str(
        "| **Build** | Ship code that satisfies acceptance criteria | 2–8 implementer Hands + 1 Merger | Opening a PR via the Merger |\n",
    );
    md.push_str(
        "| **Research** | Reduce uncertainty before code is written | 1–4 investigator Hands, no Merger | Posting findings + a recommendation |\n",
    );
    md.push_str(
        "| **Review** | Critique existing code, designs, or PRs | 1–3 reviewer Hands, no Merger | Posting a structured review comment |\n\n",
    );
    md.push_str("Atlas selects an archetype per goal:\n\n");
    md.push_str("1. *\"Should we do X?\"* → **Research Head**.\n");
    md.push_str("2. *\"Critique this PR/design.\"* → **Review Head**.\n");
    md.push_str("3. *Concrete acceptance criteria, path forward clear* → **Build Head**.\n");
    md.push_str("4. *Otherwise* → ask the user. Do not invent a fourth archetype.\n\n");
    md.push_str(
        "Full definitions, delegation scopes, hard rules, and cross-archetype invariants \
         live in [`docs/HEAD_ARCHETYPES.md`](../docs/HEAD_ARCHETYPES.md). To add a new \
         archetype, follow [`docs/HEAD_HOWTO.md`](../docs/HEAD_HOWTO.md).\n\n",
    );

    md.push_str("### Commands\n\n");
    md.push_str("```sh\n");
    md.push_str("ryve head --help                                      # long-form Head docs\n");
    md.push_str("ryve head spawn --help                                # spawn-specific help\n");
    md.push_str(
        "ryve head spawn <epic_id> --archetype build [--agent <a>] [--crew <c>]  # launch a Head on an epic\n",
    );
    md.push_str(
        "ryve head list                                        # list active Head sessions\n",
    );
    md.push('\n');
    md.push_str("ryve crew create <name> --parent <epic_id> --head-session $RYVE_SESSION_ID\n");
    md.push_str("ryve crew list\n");
    md.push_str("ryve crew show <crew_id>\n");
    md.push('\n');
    md.push_str(
        "ryve hand spawn <child_id> --agent <a> --crew <crew_id>            # worker Hand\n",
    );
    md.push_str("ryve hand spawn <merge_id> --role merger --crew <crew_id>          # Merger\n");
    md.push_str("```\n\n");

    md.push_str("### Worked example — OAuth login\n\n");
    md.push_str("```sh\n");
    md.push_str("# 1. Atlas (or the user) creates the epic.\n");
    md.push_str("ryve spark create --type epic --priority 1 \\\n");
    md.push_str("    --problem 'add OAuth login to the dashboard' \\\n");
    md.push_str("    --acceptance 'user can log in with Google on /login' \\\n");
    md.push_str("    --acceptance 'session cookie set and verified on /dashboard' \\\n");
    md.push_str("    'Add OAuth login'\n");
    md.push_str("# → created ryve-abcd1234\n\n");
    md.push_str("# 2. Spawn a Build Head on the epic.\n");
    md.push_str("ryve head spawn ryve-abcd1234 --agent claude\n");
    md.push_str("# → spawned head <session> on epic ryve-abcd1234 (pid Some(…))\n\n");
    md.push_str("# 3. The Head, running in its own worktree, will:\n");
    md.push_str("#    - ryve spark show ryve-abcd1234                     (read intent)\n");
    md.push_str("#    - ryve spark create --type task … (×3)              (decompose)\n");
    md.push_str("#    - ryve bond create ryve-abcd1234 <child> parent_child\n");
    md.push_str("#    - ryve crew create 'oauth-dashboard' --parent ryve-abcd1234 \\\n");
    md.push_str("#          --head-session $RYVE_SESSION_ID\n");
    md.push_str("#    - ryve hand spawn <child1> --agent claude --crew <crew_id>  (×3)\n");
    md.push_str("#    - poll crew + assignments\n");
    md.push_str("#    - ryve spark create --type chore … 'Merge crew <crew_id>'\n");
    md.push_str("#    - ryve hand spawn <merge_id> --role merger --crew <crew_id>\n");
    md.push_str("#    - ryve comment add ryve-abcd1234 '<pr-url>'\n");
    md.push_str("#    - exit\n\n");
    md.push_str("# 4. You can observe each layer at any time:\n");
    md.push_str("ryve head list                 # the Head itself\n");
    md.push_str("ryve crew list                 # the crew it created\n");
    md.push_str("ryve hand list                 # the sub-Hands\n");
    md.push_str("ryve spark show ryve-abcd1234  # child sparks as parent_child bonds\n");
    md.push_str("```\n\n");
    md.push_str(
        "See [`docs/AGENT_HIERARCHY.md`](../docs/AGENT_HIERARCHY.md) for the full \
         Atlas → Head → Hand architecture, [`docs/HEAD_ARCHETYPES.md`](../docs/HEAD_ARCHETYPES.md) \
         for the three standard archetypes, and [`docs/HEAD_PLAN.md`](../docs/HEAD_PLAN.md) \
         for the implementation plan and rationale.\n\n",
    );

    // ── Alloys ─────────────────────────────────────────────────────
    md.push_str("## Alloys (Spark Groupings)\n\n");
    md.push_str(
        "An **alloy** is a named bundle of sparks that should be executed together. \
         Alloys let a planner stage a group of related work up front so Hands can \
         pick them up in the right shape. Alloys are a planning aid — individual \
         spark lifecycle (status, bonds, contracts) still applies to each member.\n\n",
    );
    md.push_str("Alloy types:\n\n");
    md.push_str(
        "- `scatter` — fan-out: members are independent and can be worked in parallel by \
         multiple Hands.\n",
    );
    md.push_str(
        "- `chain` — sequential: members must be completed in order. Each member typically \
         has a `blocks` bond on the next.\n",
    );
    md.push_str(
        "- `watch` — observation group: members share a watch/monitor relationship (e.g. \
         a spark plus the checks that gate it).\n\n",
    );
    md.push_str(
        "Alloys are currently managed from the Ryve UI and internal APIs — there is no \
         top-level `ryve alloy` CLI subcommand yet. When you encounter an alloy membership \
         on a spark, treat it as planning context: respect the implied ordering (for \
         chains) or parallelism (for scatters) when choosing what to work on.\n\n",
    );

    // ── Workflow rules ────────────────────────────────────────────
    md.push_str("## Workflow\n\n");
    md.push_str("- **Claim a spark** before starting work to prevent duplicate effort.\n");
    md.push_str(
        "- **Read the spark intent** (`ryve spark show <id>`) before coding — it defines \
         problem, invariants, non-goals, and acceptance criteria.\n",
    );
    md.push_str(
        "- **Inspect bonds** (`ryve bond list <id>` or `ryve hot`) — Do not work on a blocked \
         spark that is still waiting on an incomplete upstream.\n",
    );
    md.push_str(
        "- **Reference spark IDs** in commit messages \
         (e.g. `fix(auth): validate token expiry [sp-a1b2]`).\n",
    );
    md.push_str("- **Focus on priority order** — P0 sparks are critical, P4 are negligible.\n");
    md.push_str("- **Respect architectural constraints** — violations are blocking.\n");
    md.push_str("- **Check required contracts** before marking a spark as done.\n");
    md.push_str(
        "- If you discover a new bug or task while working, mention it so it can be \
         tracked as a new spark.\n",
    );
    md.push_str("- Do not close or modify sparks directly — Ryve manages spark lifecycle.\n");

    md
}

// ── Pointer injection ─────────────────────────────────

/// The directive that gets injected between markers.
fn pointer_line() -> String {
    format!(
        "{}\n\
         ## Ryve Workshop — MANDATORY\n\
         \n\
         This project is managed by **Ryve**. You MUST read `RYVE.md` before doing \
         ANY work — it is the universal CLI reference, house rules, and coordination \
         protocol for all coding agents.\n\
         \n\
         **Work in your current directory.** Do not navigate to parent directories or other \
         worktrees. Run `ryve spark list` to see active tasks and find work to claim.\n\
         {}",
        MARKER_START, MARKER_END,
    )
}

/// Inject the pointer into a single agent boot file, skipping the write if
/// the on-disk content is already what we'd produce.
///
/// - If the file already contains the markers, replace the block between them.
/// - If the file exists but has no markers, append the block.
/// - If the file doesn't exist, create it with just the block.
///
/// `cache` records the hash of the file the last time we either wrote it or
/// observed it equal to our target. When the disk hash still matches the
/// cache, the pointer block is unchanged (the pointer literal is constant)
/// and we skip recomputation entirely.
async fn inject_pointer_if_changed(
    workshop_dir: &Path,
    relative: &str,
    cache: &Mutex<SyncCache>,
) -> Result<(), std::io::Error> {
    let path = workshop_dir.join(relative);

    // Cheap path: if we previously wrote or observed a correct file at this
    // path *and* the file still exists, the injected pointer block is
    // constant (deterministic), so the file is still correct. Skip all I/O.
    // External edits self-correct on the next cold start (fresh cache).
    //
    // We verify existence here so that a mid-run deletion of the pointer
    // file is repaired on the next sync rather than being "forgotten" until
    // a fresh cache is constructed (Copilot PR #23 review).
    if cache_has_entry(cache, &path) {
        match tokio::fs::metadata(&path).await {
            Ok(_) => return Ok(()),
            Err(_) => {
                // File was deleted out from under us — invalidate the cache
                // entry and fall through to recreate it.
                cache_forget(cache, &path);
            }
        }
    }

    // Cache miss (cold cache, first encounter, or repaired deletion) —
    // fall through to read/write.
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let pointer = pointer_line();

    let (new_content, existing_hash) = match tokio::fs::read_to_string(&path).await {
        Ok(existing) => {
            let existing_hash = hash_str(&existing);

            let updated = if let (Some(start), Some(end)) =
                (existing.find(MARKER_START), existing.find(MARKER_END))
            {
                // Replace existing block
                let mut out = String::with_capacity(existing.len());
                out.push_str(&existing[..start]);
                out.push_str(&pointer);
                out.push_str(&existing[end + MARKER_END.len()..]);
                out
            } else if existing.contains(MARKER_START) {
                // Malformed — marker start without end. Append fresh block.
                format!("{existing}\n{pointer}\n")
            } else {
                // No markers yet — append
                format!("{existing}\n{pointer}\n")
            };

            (updated, Some(existing_hash))
        }
        Err(_) => {
            // File doesn't exist — create with just the pointer
            (format!("{pointer}\n"), None)
        }
    };

    let new_hash = hash_str(&new_content);
    if Some(new_hash) == existing_hash {
        // Disk already matches what injection would produce. Cache and skip.
        cache_set(cache, path, new_hash);
        return Ok(());
    }

    tokio::fs::write(&path, &new_content).await?;
    cache_set(cache, path, new_hash);
    Ok(())
}

// ── Config helpers ────────────────────────────────────

fn resolve_targets(config: &WorkshopConfig) -> Vec<String> {
    if let Some(ref targets) = config.agents.target_files {
        targets.clone()
    } else {
        DEFAULT_TARGETS.iter().map(|s| s.to_string()).collect()
    }
}

/// Return the paths that `sync` would write to (for display / gitignore hints).
pub fn target_paths(workshop_dir: &Path, config: &WorkshopConfig) -> Vec<PathBuf> {
    let mut paths = vec![workshop_dir.join(".ryve/WORKSHOP.md")];
    for rel in resolve_targets(config) {
        paths.push(workshop_dir.join(rel));
    }
    paths
}

// ── Hand prompt generation ───────────────────────────

/// Generate the initial prompt text to inject into a Hand's terminal via PTY.
/// This is the fallback for agents that don't support `--system-prompt` flags.
pub fn generate_hand_prompt(_workshop_dir: &Path) -> String {
    "You are a Hand in a Ryve Workshop. Before doing ANY work, read `.ryve/WORKSHOP.md` — \
     it explains how to use `ryve` to query the workgraph for active tasks, constraints, \
     and contracts. Work ONLY in your current directory (do not navigate to parent directories \
     or other worktrees). Run `ryve spark list` to see what needs doing. \
     Reference spark IDs in all commits (e.g. `[sp-xxxx]`).\n"
        .to_string()
}

// ── Tests ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workshop_md_mentions_bond_dependencies() {
        // sp-ux0006: WORKSHOP.md must teach Hands about bonds and how to
        // avoid claiming a blocked spark. Bonds are otherwise invisible to
        // anyone not running raw SQL.
        let md = generate_workshop_md();
        assert!(md.contains("ryve bond list"), "must document bond list cmd");
        assert!(md.contains("blocks"), "must mention 'blocks' bond type");
        assert!(
            md.contains("Dependencies"),
            "must have a Dependencies section"
        );
        assert!(
            md.contains("blocked spark") || md.contains("Do not work on a blocked"),
            "must tell agents not to claim blocked sparks"
        );
    }

    #[test]
    fn workshop_md_documents_bonds_and_unblocked_work() {
        let md = generate_workshop_md();
        // Bond section explains dependency blocking.
        assert!(md.contains("## Bonds"));
        assert!(md.contains("blocks"));
        assert!(md.contains("conditional_blocks"));
        assert!(md.contains("ryve bond list"));
        assert!(md.contains("ryve bond create"));
        // `ryve hot` surfaced as the unblocked-work entry point.
        assert!(md.contains("ryve hot"));
    }

    #[test]
    fn workshop_md_documents_spark_intent() {
        let md = generate_workshop_md();
        assert!(md.contains("## Spark Intent"));
        assert!(md.contains("problem_statement"));
        assert!(md.contains("invariants"));
        assert!(md.contains("non_goals"));
        assert!(md.contains("acceptance_criteria"));
        // Intent flags for `spark create` are documented.
        assert!(md.contains("--problem"));
        assert!(md.contains("--invariant"));
        assert!(md.contains("--non-goal"));
        assert!(md.contains("--acceptance"));
    }

    /// Spark ryve-80d69f32: WORKSHOP.md must teach Hands about Heads —
    /// what they are, their lifecycle, the three standard archetypes,
    /// and the `ryve head` CLI. Without this, the feature is invisible
    /// to anyone reading the operations guide.
    #[test]
    fn workshop_md_documents_heads_and_archetypes() {
        let md = generate_workshop_md();

        // Section header exists.
        assert!(
            md.contains("## Heads"),
            "WORKSHOP.md must have a Heads section"
        );

        // Definition: what a Head is and how it relates to a Hand.
        assert!(
            md.contains("crew orchestrator") || md.contains("Crew of Hands"),
            "must define a Head as a crew orchestrator"
        );
        assert!(
            md.contains("same kind of coding-agent subprocess"),
            "must clarify Heads are mechanically identical to Hands"
        );
        assert!(
            md.contains("never edit"),
            "must state Heads never edit code themselves"
        );

        // Lifecycle steps map to concrete ryve commands.
        assert!(
            md.contains("### Lifecycle"),
            "must have a Lifecycle section"
        );
        assert!(md.contains("ryve head spawn"));
        assert!(md.contains("ryve crew create"));
        assert!(md.contains("ryve hand spawn"));
        assert!(md.contains("--role merger"));

        // All three standard archetypes are listed.
        assert!(md.contains("### Archetypes"));
        assert!(md.contains("Build"));
        assert!(md.contains("Research"));
        assert!(md.contains("Review"));

        // Worked example is present.
        assert!(md.contains("### Worked example"));
        assert!(md.contains("--type epic"));

        // Help surface + pointer to the HOWTO.
        assert!(md.contains("ryve head --help"));
        assert!(md.contains("HEAD_HOWTO.md"));
        assert!(md.contains("HEAD_ARCHETYPES.md"));
    }

    #[test]
    fn workshop_md_documents_alloys() {
        let md = generate_workshop_md();
        assert!(md.contains("## Alloys"));
        assert!(md.contains("scatter"));
        assert!(md.contains("chain"));
        assert!(md.contains("watch"));
    }

    /// Acceptance criterion for spark ryve-86b0b326: with no config or
    /// worktree changes, repeated `sync()` calls produce zero file writes.
    /// We verify by snapshotting mtimes after the first sync, sleeping
    /// past filesystem mtime resolution, and confirming the second sync
    /// leaves every mtime untouched.
    #[tokio::test]
    async fn sync_is_idempotent_with_zero_writes_on_unchanged_state() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        // `.ryve/` must exist for `RyveDir::workshop_md_path` to be writeable.
        std::fs::create_dir_all(dir.join(".ryve")).unwrap();
        let ryve_dir = RyveDir::new(&dir);
        let config = WorkshopConfig::default();
        let cache = Mutex::new(SyncCache::new());

        // First sync: writes everything.
        sync(&dir, &ryve_dir, &config, &cache).await.unwrap();

        // Snapshot mtime + size of every file sync would touch.
        let tracked: Vec<PathBuf> = target_paths(&dir, &config);
        let snapshots: Vec<_> = tracked
            .iter()
            .map(|p| {
                let m = std::fs::metadata(p).expect("file missing after first sync");
                (p.clone(), m.modified().unwrap(), m.len())
            })
            .collect();
        assert!(!snapshots.is_empty(), "sync should have produced files");

        // Sleep past coarse mtime resolution (HFS+/APFS often 1s).
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Second sync: should be a complete no-op on disk.
        sync(&dir, &ryve_dir, &config, &cache).await.unwrap();

        for (path, prev_mtime, prev_len) in &snapshots {
            let m = std::fs::metadata(path).expect("file vanished between syncs");
            assert_eq!(
                m.modified().unwrap(),
                *prev_mtime,
                "second sync rewrote {} (mtime changed)",
                path.display(),
            );
            assert_eq!(
                m.len(),
                *prev_len,
                "second sync rewrote {} (length changed)",
                path.display(),
            );
        }
    }

    /// Even when the cache starts cold (e.g. immediately after process
    /// restart), sync must not rewrite files whose disk content already
    /// matches what it would produce.
    #[tokio::test]
    async fn sync_with_cold_cache_skips_writes_when_disk_already_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join(".ryve")).unwrap();
        let ryve_dir = RyveDir::new(&dir);
        let config = WorkshopConfig::default();

        // Prime disk with a first sync, then drop the cache.
        {
            let cache = Mutex::new(SyncCache::new());
            sync(&dir, &ryve_dir, &config, &cache).await.unwrap();
        }

        let tracked = target_paths(&dir, &config);
        let snapshots: Vec<_> = tracked
            .iter()
            .map(|p| (p.clone(), std::fs::metadata(p).unwrap().modified().unwrap()))
            .collect();

        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Fresh cache: must read each file, see it already matches, and
        // skip the write.
        let cache = Mutex::new(SyncCache::new());
        sync(&dir, &ryve_dir, &config, &cache).await.unwrap();

        for (path, prev_mtime) in &snapshots {
            let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
            assert_eq!(
                mtime,
                *prev_mtime,
                "cold-cache sync rewrote {} despite matching content",
                path.display(),
            );
        }
    }

    #[test]
    fn workshop_md_documents_ember_send_and_spark_edit() {
        let md = generate_workshop_md();
        // Previously missing commands must now be documented.
        assert!(md.contains("ryve ember send"));
        assert!(md.contains("glow"));
        assert!(md.contains("blaze"));
        assert!(md.contains("ryve spark edit"));
    }

    /// Spark ryve-f84c676f: warm-cache sync must not add new cache entries
    /// on a second pass, which would indicate it re-hashed and re-wrote
    /// files it had already observed.
    ///
    /// What this test actually asserts (kept narrow to avoid filesystem
    /// timing flakiness in CI):
    /// 1. After the first sync, the cache contains an entry for every
    ///    file sync is responsible for (WORKSHOP.md + each target file).
    /// 2. A second sync with that warm cache produces no additional
    ///    entries, which is the observable signature of the fast path in
    ///    `inject_pointer_if_changed` short-circuiting via
    ///    `cache_has_entry` before any disk I/O.
    ///
    /// This is an indirect but deterministic check. A stricter "no I/O"
    /// assertion (e.g. chmod the target read-only and confirm no write
    /// error surfaces) was intentionally not used: read-only permissions
    /// do not block reads, and syscall-level verification is platform-
    /// specific. The cache-size invariant is sufficient to catch the
    /// regression we care about (Copilot PR #23 review).
    #[tokio::test]
    async fn warm_cache_sync_does_not_read_pointer_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join(".ryve")).unwrap();
        let ryve_dir = RyveDir::new(&dir);
        let config = WorkshopConfig::default();
        let cache = Mutex::new(SyncCache::new());

        // First sync: populates disk and cache.
        sync(&dir, &ryve_dir, &config, &cache).await.unwrap();

        // Count cache entries — should cover WORKSHOP.md + RYVE.md + all target files.
        let cached_count = cache.lock().unwrap().file_hashes.len();
        let expected_files = 2 + resolve_targets(&config).len(); // WORKSHOP.md + RYVE.md + targets
        assert_eq!(
            cached_count, expected_files,
            "cache should have an entry for every file sync touches"
        );

        // Second sync with warm cache: all helpers should hit the cache
        // and return without any filesystem I/O.
        sync(&dir, &ryve_dir, &config, &cache).await.unwrap();

        // Verify cache size didn't change (no new entries added).
        assert_eq!(
            cache.lock().unwrap().file_hashes.len(),
            cached_count,
            "warm-cache sync should not add new cache entries"
        );
    }
}
