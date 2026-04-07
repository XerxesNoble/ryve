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
         to see active sparks, their priorities, and which are already claimed.\n\n",
    );

    // ── Rules ─────────────────────────────────────────────────────
    md.push_str("## Rules\n\n");
    md.push_str("1. **Always reference spark IDs** in commit messages: `fix(auth): validate token expiry [sp-a1b2]`\n");
    md.push_str("2. **Work in priority order** — P0 is critical, P4 is negligible.\n");
    md.push_str("3. **Respect architectural constraints** — run `ryve constraint list` to check. Violations are blocking.\n");
    md.push_str("4. **Check required contracts** before considering a spark done: `ryve contract list <spark-id>`.\n");
    md.push_str("5. **Do not work on a spark that is already claimed** by another Hand.\n");
    md.push_str(
        "6. **Do not work on a blocked spark.** Before claiming a spark, run \
         `ryve bond list <spark-id>` and check that none of its incoming \
         `blocks`/`conditional_blocks` bonds reference an open (non-closed) spark. \
         Pick a different spark and come back later.\n",
    );
    md.push_str(
        "7. If you discover a new bug or task, create a spark for it (see commands below).\n\n",
    );

    // ── Workgraph commands ───────────────────────────────────────
    md.push_str("## Workgraph Commands\n\n");
    md.push_str(
        "Use `ryve` to query and update the workgraph. **Always run from the workshop root.**\n\n",
    );

    md.push_str("### Query state\n\n");
    md.push_str("```sh\nryve spark list              # active sparks\n");
    md.push_str("ryve spark list --all         # include closed\n");
    md.push_str("ryve spark show <spark-id>    # spark details\n");
    md.push_str("ryve bond list <spark-id>     # dependencies (blocks / blocked-by)\n");
    md.push_str("ryve constraint list           # architectural constraints\n");
    md.push_str("ryve contract list <spark-id>  # verification contracts\n```\n\n");

    md.push_str("### Mutate state\n\n");
    md.push_str("```sh\nryve spark create <title>                    # create a new spark\n");
    md.push_str("ryve spark status <spark-id> in_progress      # claim / update status\n");
    md.push_str("ryve spark close <spark-id> <reason>           # close a spark\n");
    md.push_str("ryve bond create <from-id> <to-id> <type>      # add dependency edge\n");
    md.push_str("ryve bond delete <bond-id>                     # remove dependency edge\n```\n\n");

    md.push_str("Ryve auto-refreshes every 3 seconds. Changes are picked up by the UI and other Hands automatically.\n\n");

    // ── Dependencies ─────────────────────────────────────────────
    md.push_str("## Dependencies (Bonds)\n\n");
    md.push_str(
        "Sparks form a directed graph through *bonds*. The two bond types you must respect are:\n\n",
    );
    md.push_str(
        "- **`blocks`** — `A blocks B` means B cannot start until A is `closed`. \
         The UI marks B with a 🔒 in the workgraph panel and a red \"Blocked by\" \
         banner in its detail view.\n",
    );
    md.push_str(
        "- **`conditional_blocks`** — same semantics for the purpose of claiming work; \
         treat as a hard block until reviewed.\n\n",
    );
    md.push_str(
        "Other bond types (`parent_child`, `related`, `waits_for`, `duplicates`, \
         `supersedes`) are informational and do not gate work.\n\n",
    );
    md.push_str("**Before claiming a spark, always check its bonds:**\n\n");
    md.push_str("```sh\nryve bond list <spark-id>\n```\n\n");
    md.push_str(
        "If any incoming `blocks`/`conditional_blocks` bond points at a spark whose \
         status is not `closed`, the spark is blocked — pick a different one. The \
         workgraph panel and `ryve spark show` will surface the same information.\n",
    );
    md.push_str("\n");

    // ── Workflow rules ────────────────────────────────────────────
    md.push_str("## Workflow\n\n");
    md.push_str("- **Claim a spark** before starting work to prevent duplicate effort.\n");
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
