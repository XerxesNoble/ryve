// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Agent context synchronisation.
//!
//! Generates `.ryve/WORKSHOP.md` — a static operations guide that tells Hands
//! how to use `ryve` to query the workgraph (the DB is the single source of
//! truth for spark state). Also injects a pointer into each agent's boot file
//! so they discover and read it automatically, and propagates both into every
//! active worktree.

use std::path::{Path, PathBuf};

use crate::ryve_dir::{RyveDir, WorkshopConfig};

// ── Marker ────────────────────────────────────────────

const MARKER_START: &str = "<!-- RYVE:START -->";
const MARKER_END: &str = "<!-- RYVE:END -->";

// ── Target files ──────────────────────────────────────

/// Well-known files that coding agents read on boot.
/// Each entry is relative to the workshop root.
const DEFAULT_TARGETS: &[&str] = &[
    "CLAUDE.md",
    "OPENCODE.md",
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
) -> Result<(), std::io::Error> {
    let workshop_md = generate_workshop_md();
    tokio::fs::write(ryve_dir.workshop_md_path(), &workshop_md).await?;

    let targets = resolve_targets(config);
    for rel in &targets {
        inject_pointer(workshop_dir, rel).await?;
    }

    // Propagate WORKSHOP.md + pointers into every active worktree
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
                let _ = tokio::fs::write(&wt_workshop_md, &workshop_md).await;
            }
            // Re-inject pointers into the worktree's agent boot files
            for rel in &targets {
                let _ = inject_pointer(&wt_path, rel).await;
            }
        }
    }

    Ok(())
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
    md.push_str("- **problem_statement** — the concrete problem the spark is solving (the *why*).\n");
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
    md.push_str("ryve hot                                   # sparks with no unmet blocking bonds\n");
    md.push_str("```\n\n");

    // ── Workgraph commands ───────────────────────────────────────
    md.push_str("## Workgraph Commands\n\n");
    md.push_str(
        "Use `ryve` to query and update the workgraph. **Always run from the workshop root.**\n\n",
    );

    md.push_str("### Query state\n\n");
    md.push_str("```sh\nryve spark list                       # active sparks\n");
    md.push_str("ryve spark list --all                 # include closed\n");
    md.push_str("ryve hot                              # sparks unblocked by bonds (ready to work)\n");
    md.push_str("ryve spark show <spark-id>            # spark details + intent\n");
    md.push_str("ryve bond list <spark-id>             # dependency bonds\n");
    md.push_str("ryve constraint list                  # architectural constraints\n");
    md.push_str("ryve contract list <spark-id>         # verification contracts\n");
    md.push_str("ryve ember list                       # live signals from other Hands / the UI\n```\n\n");

    md.push_str("### Mutate state\n\n");
    md.push_str("```sh\n");
    md.push_str("ryve spark create <title>                           # create a task spark\n");
    md.push_str("ryve spark create --type bug --priority 1 \\\n");
    md.push_str("  --problem '...' --invariant '...' \\\n");
    md.push_str("  --non-goal '...' --acceptance '...' <title>       # create with structured intent\n");
    md.push_str("ryve spark edit <spark-id> --title <t> \\\n");
    md.push_str("  --priority <0-4> --risk <level> --scope <path>    # edit fields in place\n");
    md.push_str("ryve spark status <spark-id> in_progress            # claim / update status\n");
    md.push_str("ryve spark close <spark-id> <reason>                # close a spark\n");
    md.push_str("\n");
    md.push_str("ryve bond create <from> <to> <type>                 # add dependency (blocks, related, ...)\n");
    md.push_str("ryve bond delete <bond-id>                          # remove a bond\n");
    md.push_str("\n");
    md.push_str("ryve comment add <spark-id> <body>                  # leave a note on a spark\n");
    md.push_str("ryve stamp add <spark-id> <label>                   # tag a spark\n");
    md.push_str("ryve contract add <spark-id> <kind> <description>   # add a verification contract\n");
    md.push_str("ryve contract check <contract-id> pass|fail         # record a contract result\n");
    md.push_str("\n");
    md.push_str("ryve ember send <type> <content>                    # broadcast an ember signal\n");
    md.push_str("ryve ember sweep                                    # clean up expired embers\n");
    md.push_str("```\n\n");

    md.push_str(
        "Ember types, in order of urgency: `glow` (ambient), `flash` (quick heads-up), \
         `flare` (needs attention soon), `blaze` (urgent — interrupt-worthy), `ash` \
         (archival / post-mortem).\n\n",
    );

    md.push_str("Ryve auto-refreshes every 3 seconds. Changes are picked up by the UI and other Hands automatically.\n\n");

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
        "- **Inspect bonds** (`ryve bond list <id>` or `ryve hot`) and do not start a \
         spark that is still blocked by an incomplete upstream.\n",
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
         This project is managed by **Ryve**. You MUST read `.ryve/WORKSHOP.md` before doing \
         ANY work — it contains the rules, CLI commands, and coordination protocol you must follow.\n\
         \n\
         **Work in your current directory.** Do not navigate to parent directories or other \
         worktrees. Run `ryve spark list` to see active tasks and find work to claim.\n\
         {}",
        MARKER_START, MARKER_END,
    )
}

/// Inject the pointer into a single agent boot file.
///
/// - If the file already contains the markers, replace the block between them.
/// - If the file exists but has no markers, append the block.
/// - If the file doesn't exist, create it with just the block.
async fn inject_pointer(workshop_dir: &Path, relative: &str) -> Result<(), std::io::Error> {
    let path = workshop_dir.join(relative);

    // Ensure parent directory exists (e.g. `.github/`)
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let pointer = pointer_line();

    let content = match tokio::fs::read_to_string(&path).await {
        Ok(existing) => {
            if let (Some(start), Some(end)) =
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
            }
        }
        Err(_) => {
            // File doesn't exist — create with just the pointer
            format!("{pointer}\n")
        }
    };

    tokio::fs::write(&path, content).await
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
}

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

    #[test]
    fn workshop_md_documents_alloys() {
        let md = generate_workshop_md();
        assert!(md.contains("## Alloys"));
        assert!(md.contains("scatter"));
        assert!(md.contains("chain"));
        assert!(md.contains("watch"));
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
}
