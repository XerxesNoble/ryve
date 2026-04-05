// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Agent context synchronisation.
//!
//! Generates `.ryve/WORKSHOP.md` (the single source of truth for coding agents)
//! and injects a one-line pointer into each agent's boot file so they discover
//! and read it automatically.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::ryve_dir::{RyveDir, WorkshopConfig};
use crate::sparks::types::{ArchConstraint, Contract, HandAssignment, Spark};

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

/// Aggregated context for WORKSHOP.md generation.
pub struct WorkshopContext {
    pub sparks: Vec<Spark>,
    pub constraints: Vec<(String, ArchConstraint)>,
    pub failing_contracts: Vec<Contract>,
    pub active_assignments: Vec<HandAssignment>,
}

/// Generate `.ryve/WORKSHOP.md` and inject pointers into agent boot files.
///
/// Call this after workshop initialisation and after every spark mutation.
pub async fn sync(
    workshop_dir: &Path,
    ryve_dir: &RyveDir,
    config: &WorkshopConfig,
    ctx: &WorkshopContext,
) -> Result<(), std::io::Error> {
    let workshop_md = generate_workshop_md(ctx);
    tokio::fs::write(ryve_dir.workshop_md_path(), &workshop_md).await?;

    let targets = resolve_targets(config);
    for rel in &targets {
        inject_pointer(workshop_dir, rel).await?;
    }

    Ok(())
}

// ── WORKSHOP.md generation ────────────────────────────

fn generate_workshop_md(ctx: &WorkshopContext) -> String {
    let mut md = String::with_capacity(4096);

    md.push_str("# Ryve Workshop\n\n");
    md.push_str(
        "You are working inside a **Ryve Workshop**. Ryve manages tasks (called *sparks*) \
         in an embedded workgraph stored at `.ryve/sparks.db`.\n\n",
    );

    // ── Active sparks ─────────────────────────────────────────────
    let hot: Vec<&Spark> = ctx
        .sparks
        .iter()
        .filter(|s| matches!(s.status.as_str(), "open" | "in_progress"))
        .collect();

    if hot.is_empty() {
        md.push_str("There are no active sparks right now.\n\n");
    } else {
        md.push_str("## Active Sparks\n\n");
        md.push_str("| ID | P | Risk | Type | Status | Scope | Title |\n");
        md.push_str("|----|---|------|------|--------|-------|-------|\n");
        for s in &hot {
            let risk = s.risk_level.as_deref().unwrap_or("normal");
            let scope = s.scope_boundary.as_deref().unwrap_or("");
            let _ = writeln!(
                md,
                "| `{}` | P{} | {} | {} | {} | {} | {} |",
                s.id, s.priority, risk, s.spark_type, s.status, scope, s.title,
            );
        }
        md.push('\n');
    }

    // ── Architectural constraints ─────────────────────────────────
    if !ctx.constraints.is_empty() {
        md.push_str("## Architectural Constraints\n\n");
        for (name, c) in &ctx.constraints {
            let severity = match c.severity {
                crate::sparks::types::ConstraintSeverity::Error => "ERROR",
                crate::sparks::types::ConstraintSeverity::Warning => "WARN",
                crate::sparks::types::ConstraintSeverity::Info => "INFO",
            };
            let _ = writeln!(md, "- **{}** [{}]: {}", name, severity, c.rule);
        }
        md.push('\n');
    }

    // ── Failing contracts ─────────────────────────────────────────
    if !ctx.failing_contracts.is_empty() {
        md.push_str("## Failing Contracts\n\n");
        for c in &ctx.failing_contracts {
            let _ = writeln!(
                md,
                "- `{}`: \"{}\" — {} ({})",
                c.spark_id,
                c.description,
                c.status.to_uppercase(),
                c.kind,
            );
        }
        md.push('\n');
    }

    // ── Hand assignments ──────────────────────────────────────────
    let active_assigns: Vec<&HandAssignment> = ctx
        .active_assignments
        .iter()
        .filter(|a| a.status == "active")
        .collect();
    if !active_assigns.is_empty() {
        md.push_str("## Active Hands\n\n");
        for a in &active_assigns {
            let _ = writeln!(
                md,
                "- `{}` claimed by session `{}` ({})",
                a.spark_id, a.session_id, a.role,
            );
        }
        md.push('\n');
    }

    // ── Workflow rules ────────────────────────────────────────────
    md.push_str("## Workflow\n\n");
    md.push_str(
        "- **Claim a spark** before starting work to prevent duplicate effort.\n",
    );
    md.push_str(
        "- **Reference spark IDs** in commit messages \
         (e.g. `fix(auth): validate token expiry [sp-a1b2]`).\n",
    );
    md.push_str(
        "- **Focus on priority order** — P0 sparks are critical, P4 are negligible.\n",
    );
    md.push_str(
        "- **Respect architectural constraints** — violations are blocking.\n",
    );
    md.push_str(
        "- **Check required contracts** before marking a spark as done.\n",
    );
    md.push_str(
        "- If you discover a new bug or task while working, mention it so it can be \
         tracked as a new spark.\n",
    );
    md.push_str(
        "- Do not close or modify sparks directly — Ryve manages spark lifecycle.\n",
    );

    md
}

// ── Pointer injection ─────────────────────────────────

/// The directive that gets injected between markers.
fn pointer_line() -> String {
    format!(
        "{}\n\
         ## Ryve Workshop — MANDATORY\n\
         \n\
         IMPORTANT: This project is managed by Ryve. You MUST read the file `.ryve/WORKSHOP.md` \
         before doing ANY work. It contains active tasks, architectural constraints, verification \
         contracts, and coordination rules that govern how you operate in this codebase. \
         Failure to follow the instructions in that file will result in wasted work, conflicts \
         with other agents, and constraint violations. Read it now.\n\
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
            if let (Some(start), Some(end)) = (
                existing.find(MARKER_START),
                existing.find(MARKER_END),
            ) {
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

/// Generate the initial prompt text to inject into a Hand's terminal via PTY.
/// This is the fallback for agents that don't support `--system-prompt` flags.
pub fn generate_hand_prompt(workshop_dir: &Path) -> String {
    let ws_md = workshop_dir.join(".ryve/WORKSHOP.md");
    let ws_md_rel = ".ryve/WORKSHOP.md";

    format!(
        "You are a Hand in a Ryve Workshop. Before doing ANY work, you MUST read \
         and follow the rules in `{ws_md_rel}`. It contains active tasks (sparks), \
         architectural constraints, verification contracts, and coordination rules. \
         Reference spark IDs in all commits (e.g. `[sp-xxxx]`). \
         Read {ws_md_rel} now.\n"
    )
}
