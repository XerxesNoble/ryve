// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Command-line interface for workgraph operations.
//!
//! Invoked via `ryve <command>` (dispatched from `main.rs` when the first
//! argument is a known CLI subcommand). Designed for use by Hands (coding
//! agents) and humans from the terminal. Operates on the `.ryve/sparks.db`
//! found by walking up from cwd or honoring `$RYVE_WORKSHOP_ROOT`.
//!
//! Supports `--json` flag on most commands for machine-parseable output.

use std::path::{Path, PathBuf};
use std::process;

use data::ryve_dir::RyveDir;
use data::sparks::types::*;
use data::sparks::{
    agent_session_repo, assignment_repo, bond_repo, comment_repo, commit_link_repo,
    constraint_helpers, contract_repo, crew_repo, ember_repo, event_repo, spark_repo, stamp_repo,
};

use crate::coding_agents::{self, CodingAgent};
use crate::hand_spawn::{self, HandKind};
use crate::worktree_cleanup::{
    self, PruneCandidate, PruneSummary, WorktreeFacts, WorktreeStatus, classify_worktree,
};

/// Known CLI subcommands. If the first non-flag argument matches one of
/// these, `main.rs` dispatches to `cli::run` instead of launching the UI.
pub const CLI_COMMANDS: &[&str] = &[
    "spark",
    "sparks",
    "bond",
    "bonds",
    "comment",
    "comments",
    "stamp",
    "stamps",
    "contract",
    "contracts",
    "constraint",
    "constraints",
    "ember",
    "embers",
    "event",
    "events",
    "assign",
    "assignment",
    "commit",
    "commits",
    "crew",
    "crews",
    "hand",
    "hands",
    "head",
    "heads",
    "worktree",
    "worktrees",
    "wt",
    "hot",
    "status",
    "init",
    "backup",
    "backups",
    "restore",
    "help",
    "--help",
    "-h",
];

pub async fn run(args: Vec<String>) {
    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    // Global --json flag
    let json_mode = args.iter().any(|a| a == "--json");
    let args_clean: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--json")
        .cloned()
        .collect();

    if matches!(
        args_clean.get(1).map(|s| s.as_str()),
        Some("help" | "--help" | "-h")
    ) {
        print_usage();
        return;
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Special: `init` doesn't need an existing DB — initialises in cwd
    if args_clean.get(1).map(|s| s.as_str()) == Some("init") {
        let ryve_dir = RyveDir::new(&cwd);
        handle_init(&ryve_dir, &cwd).await;
        return;
    }

    // Special: `restore` must NOT open the sparks database first — it's
    // about to replace it. Resolve the workshop root ourselves and
    // dispatch without opening a pool. If the live database is missing
    // or corrupted, the standard dispatch path below would die before
    // restore could run, which is exactly the scenario we need to
    // support.
    if args_clean.get(1).map(|s| s.as_str()) == Some("restore") {
        let workshop_root = match resolve_workshop_root_for_restore(&cwd) {
            Some(r) => r,
            None => die(
                "no .ryve/ directory found — run `ryve init` first or pass an absolute snapshot path inside a workshop",
            ),
        };
        let ryve_dir = RyveDir::new(&workshop_root);
        handle_restore(&ryve_dir, &args_clean[2..]).await;
        return;
    }

    // Find the workshop root by walking up the directory tree, or honor
    // $RYVE_WORKSHOP_ROOT if set. This lets Hands run `ryve` from inside
    // a worktree without needing to cd to the workshop root first.
    let workshop_root = match std::env::var("RYVE_WORKSHOP_ROOT").ok() {
        Some(root) => PathBuf::from(root),
        None => find_workshop_root(&cwd).unwrap_or_else(|| {
            die("no .ryve/sparks.db found in current directory or any parent. Run `ryve init` or use a Ryve workshop.");
        }),
    };

    let ryve_dir = RyveDir::new(&workshop_root);
    if !ryve_dir.sparks_db_path().exists() {
        die(&format!(
            "no .ryve/sparks.db at {}",
            workshop_root.display()
        ));
    }

    let pool = match data::db::open_sparks_db(&workshop_root).await {
        Ok(p) => p,
        Err(e) => die(&format!("failed to open database: {e}")),
    };

    let ws_id = workshop_root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    match args_clean[1].as_str() {
        "spark" | "sparks" => handle_spark(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "bond" | "bonds" => handle_bond(&pool, &args_clean[2..], json_mode).await,
        "comment" | "comments" => handle_comment(&pool, &args_clean[2..], json_mode).await,
        "stamp" | "stamps" => handle_stamp(&pool, &args_clean[2..]).await,
        "contract" | "contracts" => {
            handle_contract(&pool, &args_clean[2..], &ws_id, json_mode).await
        }
        "constraint" | "constraints" => {
            handle_constraint(&pool, &args_clean[2..], &ws_id, json_mode).await
        }
        "ember" | "embers" => handle_ember(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "event" | "events" => handle_event(&pool, &args_clean[2..], json_mode).await,
        "assign" | "assignment" => handle_assignment(&pool, &args_clean[2..], json_mode).await,
        "commit" | "commits" => handle_commit(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "crew" | "crews" => handle_crew(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "hand" | "hands" => handle_hand(&pool, &workshop_root, &args_clean[2..], json_mode).await,
        "head" | "heads" => handle_head(&pool, &workshop_root, &args_clean[2..], json_mode).await,
        "worktree" | "worktrees" | "wt" => {
            handle_worktree(&pool, &workshop_root, &args_clean[2..], json_mode).await
        }
        "hot" => handle_hot(&pool, &ws_id, json_mode).await,
        "status" => handle_status(&pool, &ws_id).await,
        "backup" | "backups" => {
            handle_backup(&pool, &workshop_root, &args_clean[2..], json_mode).await
        }
        other => {
            eprintln!("error: unknown command '{other}'");
            print_usage();
            process::exit(1);
        }
    }
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    process::exit(1);
}

/// Walk up the directory tree from `start` looking for a directory
/// that contains a `.ryve/sparks.db`. Returns the workshop root path.
///
/// Special handling: if we're inside a `.ryve/worktrees/<id>/` subtree,
/// the workshop root is the parent of the `.ryve/` directory, not the
/// worktree itself (which has no `.ryve/sparks.db` of its own).
fn find_workshop_root(start: &std::path::Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().ok()?;
    loop {
        let candidate = current.join(".ryve").join("sparks.db");
        if candidate.exists() {
            return Some(current);
        }
        // If `current` itself is inside a `.ryve/` dir (e.g. a worktree),
        // jumping to `current.parent()` may still be inside `.ryve/`; keep
        // walking until we find a real workshop root.
        current = current.parent()?.to_path_buf();
    }
}

fn print_usage() {
    eprintln!("ryve — workgraph operations for Ryve workshops\n");
    eprintln!("USAGE: ryve [--json] <command> <subcommand> [args...]\n");
    eprintln!("       (with no arguments, launches the Ryve UI)\n");
    eprintln!("COMMANDS:");
    eprintln!("  init                                Initialize .ryve/ in current directory");
    eprintln!("  status                              Show workshop summary");
    eprintln!("  hot                                 List hot (ready-to-work) sparks");
    eprintln!();
    eprintln!("  backup create                       Snapshot sparks.db to .ryve/backups/");
    eprintln!("  backup list                         List existing snapshots");
    eprintln!("  backup prune [--keep=N]             Prune old snapshots (keep newest N)");
    eprintln!("  restore <snapshot>                  Restore sparks.db from a snapshot");
    eprintln!();
    eprintln!("  spark list [--all]                  List sparks (active by default)");
    eprintln!("  spark create <title>                Create a task spark (P2)");
    eprintln!("  spark create --type bug --priority 0 --problem 'desc' <title>");
    eprintln!("  spark create --help                 Show all create flags (intent, risk, etc.)");
    eprintln!("  spark show <id>                     Show spark details + intent");
    eprintln!("  spark status <id> <new_status>      Update status");
    eprintln!("  spark close <id> [reason]           Close a spark");
    eprintln!("  spark edit <id> --title <t> --priority <p> --risk <r> --scope <s>");
    eprintln!();
    eprintln!("  bond create <from> <to> <type>      Create dependency (blocks, related, etc.)");
    eprintln!("  bond list <spark_id>                List bonds for a spark");
    eprintln!("  bond delete <id>                    Delete a bond");
    eprintln!();
    eprintln!("  comment add <spark_id> <body>       Add a comment");
    eprintln!("  comment list <spark_id>             List comments");
    eprintln!();
    eprintln!("  stamp add <spark_id> <label>        Add a label");
    eprintln!("  stamp remove <spark_id> <label>     Remove a label");
    eprintln!("  stamp list <spark_id>               List labels");
    eprintln!();
    eprintln!("  contract list <spark_id>            List contracts");
    eprintln!("  contract add <spark_id> <kind> <description>");
    eprintln!("  contract check <contract_id> pass|fail");
    eprintln!("  contract failing                    List all failing required contracts");
    eprintln!();
    eprintln!("  constraint list                     List architectural constraints");
    eprintln!();
    eprintln!("  ember list                          List active embers");
    eprintln!("  ember send <type> <content>         Send an ember (flash, flare, blaze)");
    eprintln!("  ember sweep                         Clean up expired embers");
    eprintln!();
    eprintln!("  event list <spark_id>               List audit trail for a spark");
    eprintln!();
    eprintln!("  assign claim <session_id> <spark_id>  Claim a spark");
    eprintln!("  assign release <session_id> <spark_id>  Release a claim");
    eprintln!("  assign list <spark_id>              Show who owns a spark");
    eprintln!();
    eprintln!("  commit link <spark_id> <hash>       Link a commit to a spark");
    eprintln!("  commit list <spark_id>              List commits for a spark");
    eprintln!("  commit scan                         Scan git log for [sp-xxxx] references");
    eprintln!();
    eprintln!("  crew create <name> [--purpose <t>] [--parent <spark_id>] [--head-session <id>]");
    eprintln!("  crew list                           List crews in this workshop");
    eprintln!("  crew show <crew_id>                 Show a crew + its members");
    eprintln!("  crew add-member <crew_id> <session_id> [--role hand|merger]");
    eprintln!("  crew remove-member <crew_id> <session_id>");
    eprintln!("  crew status <crew_id> active|merging|completed|abandoned");
    eprintln!();
    eprintln!("  hand spawn <spark_id> [--agent <name>] [--role owner|head|merger] [--crew <id>]");
    eprintln!("                                       Spawn a Hand subprocess on a spark");
    eprintln!("  hand list                            List active hand assignments");
    eprintln!();
    eprintln!("  head spawn <epic_id> [--agent <name>] [--crew <id>]");
    eprintln!("                                       Spawn a Head (crew orchestrator) on an epic");
    eprintln!("  head list                            List active Head sessions");
    eprintln!("  head --help                          Long-form Head documentation");
    eprintln!();
    eprintln!(
        "  worktree prune [--yes]               Prune stale hand worktrees (dry-run by default)"
    );
    eprintln!("  wt prune                             Alias for worktree prune");
    eprintln!();
    eprintln!("FLAGS:");
    eprintln!("  --json    Output as JSON (for machine consumption)");
    eprintln!();
    eprintln!("Run from a Ryve workshop root (directory containing .ryve/).");
}

// ── Init ─────────────────────────────────────────────

async fn handle_init(ryve_dir: &RyveDir, cwd: &Path) {
    if let Err(e) = data::ryve_dir::init_ryve_dir(ryve_dir).await {
        die(&format!("failed to initialize: {e}"));
    }
    if let Err(e) = data::db::open_sparks_db(cwd).await {
        die(&format!("failed to create database: {e}"));
    }
    println!("initialized .ryve/ in {}", cwd.display());
}

// ── Backup / Restore ─────────────────────────────────

/// Walk up from `start` looking for a directory that contains a
/// `.ryve/` dir — used by `restore` because the live sparks.db may be
/// missing or corrupt (so [`find_workshop_root`]'s existence check on
/// the DB file itself won't work).
fn resolve_workshop_root_for_restore(start: &std::path::Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().ok()?;
    loop {
        if current.join(".ryve").is_dir() {
            return Some(current);
        }
        current = current.parent()?.to_path_buf();
    }
}

async fn handle_backup(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    args: &[String],
    json_mode: bool,
) {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("create");
    let ryve_dir = RyveDir::new(workshop_root);
    match sub {
        "create" | "now" | "snapshot" => {
            match data::backup::snapshot_and_retain(
                pool,
                &ryve_dir,
                data::backup::DEFAULT_BACKUP_RETENTION,
            )
            .await
            {
                Ok(path) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::json!({ "snapshot": path.display().to_string() })
                        );
                    } else {
                        println!("snapshot written: {}", path.display());
                    }
                }
                Err(e) => die(&format!("backup failed: {e}")),
            }
        }
        "list" | "ls" => match data::backup::list_snapshots(&ryve_dir).await {
            Ok(snaps) => {
                if json_mode {
                    let json: Vec<_> = snaps
                        .iter()
                        .map(|s| {
                            serde_json::json!({
                                "path": s.path.display().to_string(),
                                "name": s.file_name(),
                                "taken_at": s.taken_at.map(|t| t.to_rfc3339()),
                                "size": s.size,
                            })
                        })
                        .collect();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json).unwrap_or_default()
                    );
                } else if snaps.is_empty() {
                    println!("No snapshots in {}", ryve_dir.backups_dir().display());
                } else {
                    println!("{:<40} {:>10}  TAKEN", "NAME", "SIZE");
                    println!("{}", "-".repeat(72));
                    for s in snaps.iter().rev() {
                        let taken = s
                            .taken_at
                            .map(|t| t.to_rfc3339())
                            .unwrap_or_else(|| "(unparsed)".to_string());
                        println!("{:<40} {:>10}  {}", s.file_name(), s.size, taken);
                    }
                }
            }
            Err(e) => die(&format!("list failed: {e}")),
        },
        "prune" => {
            let keep = args
                .iter()
                .skip(1)
                .find_map(|a| a.strip_prefix("--keep="))
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(data::backup::DEFAULT_BACKUP_RETENTION);
            match data::backup::apply_retention(&ryve_dir, keep).await {
                Ok(deleted) => {
                    if json_mode {
                        let json: Vec<_> =
                            deleted.iter().map(|p| p.display().to_string()).collect();
                        println!("{}", serde_json::to_string(&json).unwrap_or_default());
                    } else {
                        println!("pruned {} snapshot(s) (keep={keep})", deleted.len());
                    }
                }
                Err(e) => die(&format!("prune failed: {e}")),
            }
        }
        "--help" | "-h" | "help" => {
            eprintln!("backup [create|list|prune]\n");
            eprintln!("  backup create                Take a snapshot now + prune to retention");
            eprintln!("  backup list                  List all snapshots in .ryve/backups/");
            eprintln!(
                "  backup prune [--keep=N]      Prune old snapshots (default keep={})",
                data::backup::DEFAULT_BACKUP_RETENTION
            );
        }
        other => die(&format!("unknown backup subcommand '{other}'")),
    }
}

async fn handle_restore(ryve_dir: &RyveDir, args: &[String]) {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        eprintln!("restore <snapshot>\n");
        eprintln!("  <snapshot>  Either a filename inside .ryve/backups/ or an absolute path");
        eprintln!();
        eprintln!("The current sparks.db (and its WAL/SHM sidecars) are moved aside to");
        eprintln!("sparks.db.pre-restore-<stamp>.bak before the snapshot is copied into place,");
        eprintln!("so a bad restore can be undone manually.");
        eprintln!();
        eprintln!("IMPORTANT: shut down the Ryve UI for this workshop before running restore.");
        if args.is_empty() {
            process::exit(1);
        }
        return;
    }
    let snapshot = PathBuf::from(&args[0]);
    match data::backup::restore_snapshot(ryve_dir, &snapshot).await {
        Ok(outcome) => {
            println!("restored {}", outcome.restored_db.display());
            println!("  from:     {}", outcome.snapshot.display());
            if let Some(prev) = outcome.previous_db_backup {
                println!("  previous: {}", prev.display());
                println!("  (kept as a safety copy — delete once you've verified the restore)");
            } else {
                println!("  previous: <no existing sparks.db>");
            }
        }
        Err(e) => die(&format!("restore failed: {e}")),
    }
}

// ── Status ───────────────────────────────────────────

async fn handle_status(pool: &sqlx::SqlitePool, ws_id: &str) {
    let all = spark_repo::list(pool, SparkFilter::default())
        .await
        .unwrap_or_default();
    let open = all.iter().filter(|s| s.status == "open").count();
    let in_progress = all.iter().filter(|s| s.status == "in_progress").count();
    let blocked = all.iter().filter(|s| s.status == "blocked").count();
    let closed = all.iter().filter(|s| s.status == "closed").count();

    let failing = contract_repo::list_failing(pool, ws_id)
        .await
        .unwrap_or_default();
    let constraints = constraint_helpers::list(pool, ws_id)
        .await
        .unwrap_or_default();

    println!("Workshop: {ws_id}");
    println!(
        "Sparks:   {} open, {} in progress, {} blocked, {} closed ({} total)",
        open,
        in_progress,
        blocked,
        closed,
        all.len()
    );
    println!("Contracts: {} failing/pending", failing.len());
    println!("Constraints: {} defined", constraints.len());
}

// ── Hot ──────────────────────────────────────────────

async fn handle_hot(pool: &sqlx::SqlitePool, ws_id: &str, json_mode: bool) {
    match data::sparks::graph::hot_sparks(pool, ws_id).await {
        Ok(sparks) => {
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&sparks).unwrap_or_default()
                );
            } else if sparks.is_empty() {
                println!("No hot sparks (all blocked, deferred, or closed).");
            } else {
                println!("{:<8} {:<3} {:<12} TITLE", "ID", "P", "TYPE");
                println!("{}", "-".repeat(60));
                for s in &sparks {
                    println!(
                        "{:<8} P{:<1} {:<12} {}",
                        s.id, s.priority, s.spark_type, s.title
                    );
                }
            }
        }
        Err(e) => die(&format!("{e}")),
    }
}

// ── Spark ────────────────────────────────────────────

async fn handle_spark(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() {
        die("spark subcommand required (list, create, show, status, close, edit)");
    }

    match args[0].as_str() {
        "list" | "ls" => {
            let show_all = args.iter().any(|a| a == "--all" || a == "-a");
            let filter = if show_all {
                SparkFilter::default()
            } else {
                SparkFilter {
                    status: Some(vec![
                        SparkStatus::Open,
                        SparkStatus::InProgress,
                        SparkStatus::Blocked,
                    ]),
                    ..Default::default()
                }
            };
            match spark_repo::list(pool, filter).await {
                Ok(sparks) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&sparks).unwrap_or_default()
                        );
                    } else if sparks.is_empty() {
                        println!("No sparks found.");
                    } else {
                        println!(
                            "{:<8} {:<3} {:<8} {:<12} {:<12} TITLE",
                            "ID", "P", "RISK", "TYPE", "STATUS"
                        );
                        println!("{}", "-".repeat(72));
                        for s in &sparks {
                            let risk = s.risk_level.as_deref().unwrap_or("normal");
                            println!(
                                "{:<8} P{:<1} {:<8} {:<12} {:<12} {}",
                                s.id, s.priority, risk, s.spark_type, s.status, s.title
                            );
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "create" => {
            if args.iter().any(|a| a == "--help" || a == "-h") {
                eprintln!("spark create [flags] <title words...>\n");
                eprintln!("FLAGS:");
                eprintln!(
                    "  --type, -t <type>           bug|feature|task|epic|chore|spike|milestone (default: task)"
                );
                eprintln!("  --priority, -p <0-4>        P0=critical, P4=negligible (default: 2)");
                eprintln!("  --risk, -r <level>          trivial|normal|elevated|critical");
                eprintln!("  --scope, -s <boundary>      Scope boundary (e.g. 'src/auth/')");
                eprintln!("  --description, -d <text>    Description");
                eprintln!("  --problem <text>            Intent: problem being solved");
                eprintln!(
                    "  --invariant <text>          Intent: invariant to preserve (repeatable)"
                );
                eprintln!("  --non-goal <text>           Intent: non-goal (repeatable)");
                eprintln!(
                    "  --acceptance <text>         Intent: acceptance criterion (repeatable)"
                );
                return;
            }
            let mut spark_type = SparkType::Task;
            let mut priority = 2i32;
            let mut risk = None;
            let mut scope = None;
            let mut description = String::new();
            let mut problem: Option<String> = None;
            let mut invariants: Vec<String> = Vec::new();
            let mut non_goals: Vec<String> = Vec::new();
            let mut acceptance: Vec<String> = Vec::new();
            let mut title_parts: Vec<&str> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--type" | "-t" => {
                        i += 1;
                        if i < args.len() {
                            spark_type = parse_spark_type(&args[i]);
                        }
                    }
                    "--priority" | "-p" => {
                        i += 1;
                        if i < args.len() {
                            priority = args[i].parse().unwrap_or(2);
                        }
                    }
                    "--risk" | "-r" => {
                        i += 1;
                        if i < args.len() {
                            risk = Some(parse_risk_level(&args[i]));
                        }
                    }
                    "--scope" | "-s" => {
                        i += 1;
                        if i < args.len() {
                            scope = Some(args[i].clone());
                        }
                    }
                    "--description" | "-d" => {
                        i += 1;
                        if i < args.len() {
                            description = args[i].clone();
                        }
                    }
                    "--problem" => {
                        i += 1;
                        if i < args.len() {
                            problem = Some(args[i].clone());
                        }
                    }
                    "--invariant" => {
                        i += 1;
                        if i < args.len() {
                            invariants.push(args[i].clone());
                        }
                    }
                    "--non-goal" => {
                        i += 1;
                        if i < args.len() {
                            non_goals.push(args[i].clone());
                        }
                    }
                    "--acceptance" => {
                        i += 1;
                        if i < args.len() {
                            acceptance.push(args[i].clone());
                        }
                    }
                    _ => title_parts.push(&args[i]),
                }
                i += 1;
            }
            let title = title_parts.join(" ");
            if title.is_empty() {
                die("spark create requires a title. Use `spark create --help` for options.");
            }

            // Build metadata JSON with intent if any intent fields provided
            let metadata = if problem.is_some()
                || !invariants.is_empty()
                || !non_goals.is_empty()
                || !acceptance.is_empty()
            {
                let intent = serde_json::json!({
                    "intent": {
                        "problem_statement": problem,
                        "invariants": invariants,
                        "non_goals": non_goals,
                        "acceptance_criteria": acceptance,
                    }
                });
                Some(intent.to_string())
            } else {
                None
            };

            let new = NewSpark {
                title: title.clone(),
                description,
                spark_type,
                priority,
                workshop_id: ws_id.to_string(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata,
                risk_level: risk,
                scope_boundary: scope,
            };
            match spark_repo::create(pool, new).await {
                Ok(spark) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&spark).unwrap_or_default()
                        );
                    } else {
                        println!("created {} — {}", spark.id, title);
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "show" => {
            if args.len() < 2 {
                die("spark show requires <id>");
            }
            match spark_repo::get(pool, &args[1]).await {
                Ok(s) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
                    } else {
                        println!("ID:          {}", s.id);
                        println!("Title:       {}", s.title);
                        println!("Status:      {}", s.status);
                        println!("Priority:    P{}", s.priority);
                        println!("Type:        {}", s.spark_type);
                        println!(
                            "Risk:        {}",
                            s.risk_level.as_deref().unwrap_or("normal")
                        );
                        if let Some(ref v) = s.scope_boundary {
                            println!("Scope:       {v}");
                        }
                        if !s.description.is_empty() {
                            println!("Description: {}", s.description);
                        }
                        if let Some(ref a) = s.assignee {
                            println!("Assignee:    {a}");
                        }
                        println!("Created:     {}", s.created_at);
                        println!("Updated:     {}", s.updated_at);
                        if let Some(ref c) = s.closed_at {
                            println!("Closed:      {c}");
                            println!("Reason:      {}", s.closed_reason.as_deref().unwrap_or(""));
                        }
                        let intent = s.intent();
                        if let Some(ref p) = intent.problem_statement {
                            println!("\nProblem:     {p}");
                        }
                        if !intent.invariants.is_empty() {
                            println!("Invariants:");
                            for inv in &intent.invariants {
                                println!("  - {inv}");
                            }
                        }
                        if !intent.non_goals.is_empty() {
                            println!("Non-goals:");
                            for ng in &intent.non_goals {
                                println!("  - {ng}");
                            }
                        }
                        if !intent.acceptance_criteria.is_empty() {
                            println!("Acceptance:");
                            for ac in &intent.acceptance_criteria {
                                println!("  - {ac}");
                            }
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "status" => {
            if args.len() < 3 {
                die("spark status requires <id> <new_status>");
            }
            let status = SparkStatus::from_str(&args[2]).unwrap_or_else(|| {
                die(&format!(
                    "invalid status '{}' (open, in_progress, blocked, deferred, closed)",
                    args[2]
                ))
            });
            let upd = UpdateSpark {
                status: Some(status),
                ..Default::default()
            };
            match spark_repo::update(pool, &args[1], upd, "cli").await {
                Ok(s) => println!("{} -> {}", s.id, s.status),
                Err(e) => die(&format!("{e}")),
            }
        }
        "close" => {
            if args.len() < 2 {
                die("spark close requires <id>");
            }
            let reason = if args.len() > 2 {
                args[2..].join(" ")
            } else {
                "completed".to_string()
            };
            match spark_repo::close(pool, &args[1], &reason, "cli").await {
                Ok(s) => println!("{} closed — {reason}", s.id),
                Err(e) => die(&format!("{e}")),
            }
        }
        "edit" => {
            if args.len() < 2 {
                die("spark edit requires <id>");
            }
            let id = &args[1];
            let mut upd = UpdateSpark::default();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--title" => {
                        i += 1;
                        if i < args.len() {
                            upd.title = Some(args[i].clone());
                        }
                    }
                    "--priority" => {
                        i += 1;
                        if i < args.len() {
                            upd.priority = Some(args[i].parse().unwrap_or(2));
                        }
                    }
                    "--risk" => {
                        i += 1;
                        if i < args.len() {
                            upd.risk_level = Some(parse_risk_level(&args[i]));
                        }
                    }
                    "--scope" => {
                        i += 1;
                        if i < args.len() {
                            upd.scope_boundary = Some(Some(args[i].clone()));
                        }
                    }
                    "--type" => {
                        i += 1;
                        if i < args.len() {
                            upd.spark_type = Some(parse_spark_type(&args[i]));
                        }
                    }
                    "--description" => {
                        i += 1;
                        if i < args.len() {
                            upd.description = Some(args[i].clone());
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            match spark_repo::update(pool, id, upd, "cli").await {
                Ok(s) => println!("{} updated", s.id),
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown spark subcommand '{other}'")),
    }
}

// ── Bond ─────────────────────────────────────────────

async fn handle_bond(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() {
        die("bond subcommand required (create, list, delete)");
    }
    match args[0].as_str() {
        "create" => {
            if args.len() < 4 {
                die("bond create requires <from_id> <to_id> <type>");
            }
            let bond_type = parse_bond_type(&args[3]);
            match bond_repo::create(pool, &args[1], &args[2], bond_type).await {
                Ok(b) => println!(
                    "bond {} created: {} --[{}]--> {}",
                    b.id, b.from_id, b.bond_type, b.to_id
                ),
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            if args.len() < 2 {
                die("bond list requires <spark_id>");
            }
            match bond_repo::list_for_spark(pool, &args[1]).await {
                Ok(bonds) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&bonds).unwrap_or_default()
                        );
                    } else if bonds.is_empty() {
                        println!("No bonds for {}.", args[1]);
                    } else {
                        for b in &bonds {
                            println!("{}: {} --[{}]--> {}", b.id, b.from_id, b.bond_type, b.to_id);
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "delete" => {
            if args.len() < 2 {
                die("bond delete requires <id>");
            }
            let id: i64 = args[1]
                .parse()
                .unwrap_or_else(|_| die("bond id must be a number"));
            match bond_repo::delete(pool, id).await {
                Ok(()) => println!("bond {id} deleted"),
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown bond subcommand '{other}'")),
    }
}

// ── Comment ──────────────────────────────────────────

async fn handle_comment(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() {
        die("comment subcommand required (add, list)");
    }
    match args[0].as_str() {
        "add" => {
            if args.len() < 3 {
                die("comment add requires <spark_id> <body>");
            }
            let body = args[2..].join(" ");
            let new = NewComment {
                spark_id: args[1].clone(),
                author: "cli".to_string(),
                body: body.clone(),
            };
            match comment_repo::create(pool, new).await {
                Ok(c) => println!("{} added to {}", c.id, args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            if args.len() < 2 {
                die("comment list requires <spark_id>");
            }
            match comment_repo::list_for_spark(pool, &args[1]).await {
                Ok(comments) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&comments).unwrap_or_default()
                        );
                    } else if comments.is_empty() {
                        println!("No comments on {}.", args[1]);
                    } else {
                        for c in &comments {
                            println!("[{}] {} ({}): {}", c.created_at, c.author, c.id, c.body);
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown comment subcommand '{other}'")),
    }
}

// ── Stamp ────────────────────────────────────────────

async fn handle_stamp(pool: &sqlx::SqlitePool, args: &[String]) {
    if args.is_empty() {
        die("stamp subcommand required (add, remove, list)");
    }
    match args[0].as_str() {
        "add" => {
            if args.len() < 3 {
                die("stamp add requires <spark_id> <label>");
            }
            match stamp_repo::add(pool, &args[1], &args[2]).await {
                Ok(()) => println!("stamp '{}' added to {}", args[2], args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "remove" => {
            if args.len() < 3 {
                die("stamp remove requires <spark_id> <label>");
            }
            match stamp_repo::remove(pool, &args[1], &args[2]).await {
                Ok(()) => println!("stamp '{}' removed from {}", args[2], args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            if args.len() < 2 {
                die("stamp list requires <spark_id>");
            }
            match stamp_repo::list_for_spark(pool, &args[1]).await {
                Ok(stamps) => {
                    if stamps.is_empty() {
                        println!("No stamps on {}.", args[1]);
                    } else {
                        println!(
                            "{}",
                            stamps
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown stamp subcommand '{other}'")),
    }
}

// ── Contract ─────────────────────────────────────────

async fn handle_contract(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() {
        die("contract subcommand required (list, add, check, failing)");
    }
    match args[0].as_str() {
        "list" | "ls" => {
            if args.len() < 2 {
                die("contract list requires <spark_id>");
            }
            match contract_repo::list_for_spark(pool, &args[1]).await {
                Ok(contracts) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&contracts).unwrap_or_default()
                        );
                    } else if contracts.is_empty() {
                        println!("No contracts for {}.", args[1]);
                    } else {
                        println!(
                            "{:<4} {:<14} {:<10} {:<8} DESCRIPTION",
                            "ID", "KIND", "ENFORCE", "STATUS"
                        );
                        println!("{}", "-".repeat(65));
                        for c in &contracts {
                            println!(
                                "{:<4} {:<14} {:<10} {:<8} {}",
                                c.id, c.kind, c.enforcement, c.status, c.description
                            );
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "add" => {
            if args.len() < 4 {
                die("contract add requires <spark_id> <kind> <description>");
            }
            let kind = parse_contract_kind(&args[2]);
            let desc = args[3..].join(" ");
            let new = NewContract {
                spark_id: args[1].clone(),
                kind,
                description: desc,
                check_command: None,
                pattern: None,
                file_glob: None,
                enforcement: ContractEnforcement::Required,
            };
            match contract_repo::create(pool, new).await {
                Ok(c) => println!("contract {} created on {}", c.id, args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "check" => {
            if args.len() < 3 {
                die("contract check requires <contract_id> <pass|fail|skip>");
            }
            let id: i64 = args[1]
                .parse()
                .unwrap_or_else(|_| die("contract id must be a number"));
            let status = match args[2].as_str() {
                "pass" => ContractStatus::Pass,
                "fail" => ContractStatus::Fail,
                "skip" | "skipped" => ContractStatus::Skipped,
                other => die(&format!("invalid status '{other}' (pass, fail, skip)")),
            };
            match contract_repo::update_status(pool, id, status, "cli").await {
                Ok(()) => println!("contract {id} -> {}", args[2]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "failing" => match contract_repo::list_failing(pool, ws_id).await {
            Ok(contracts) => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&contracts).unwrap_or_default()
                    );
                } else if contracts.is_empty() {
                    println!("No failing contracts.");
                } else {
                    for c in &contracts {
                        println!(
                            "[{}] {} on {} — {}",
                            c.status.to_uppercase(),
                            c.kind,
                            c.spark_id,
                            c.description
                        );
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        },
        other => die(&format!("unknown contract subcommand '{other}'")),
    }
}

// ── Constraint ───────────────────────────────────────

async fn handle_constraint(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() || args[0] == "list" || args[0] == "ls" {
        match constraint_helpers::list(pool, ws_id).await {
            Ok(constraints) => {
                if json_mode {
                    let map: Vec<_> = constraints
                        .iter()
                        .map(|(n, c)| serde_json::json!({"name": n, "constraint": c}))
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&map).unwrap_or_default());
                } else if constraints.is_empty() {
                    println!("No architectural constraints defined.");
                } else {
                    for (name, c) in &constraints {
                        let sev = match c.severity {
                            ConstraintSeverity::Error => "ERROR",
                            ConstraintSeverity::Warning => "WARN",
                            ConstraintSeverity::Info => "INFO",
                        };
                        println!("[{sev}] {name} — {}", c.rule);
                        if let Some(ref r) = c.rationale {
                            println!("  rationale: {r}");
                        }
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        }
    }
}

// ── Ember ────────────────────────────────────────────

async fn handle_ember(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() {
        die("ember subcommand required (list, send, sweep)");
    }
    match args[0].as_str() {
        "list" | "ls" => match ember_repo::list_active(pool, ws_id).await {
            Ok(embers) => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&embers).unwrap_or_default()
                    );
                } else if embers.is_empty() {
                    println!("No active embers.");
                } else {
                    for e in &embers {
                        println!(
                            "[{}] {} (from: {}, ttl: {}s): {}",
                            e.id,
                            e.ember_type,
                            e.source_agent.as_deref().unwrap_or("?"),
                            e.ttl_seconds,
                            e.content
                        );
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        },
        "send" => {
            if args.len() < 3 {
                die("ember send requires <type> <content>");
            }
            let ember_type = match args[1].as_str() {
                "glow" => EmberType::Glow,
                "flash" => EmberType::Flash,
                "flare" => EmberType::Flare,
                "blaze" => EmberType::Blaze,
                "ash" => EmberType::Ash,
                other => die(&format!(
                    "invalid ember type '{other}' (glow, flash, flare, blaze, ash)"
                )),
            };
            let content = args[2..].join(" ");
            let new = NewEmber {
                ember_type,
                content,
                source_agent: Some("cli".to_string()),
                workshop_id: ws_id.to_string(),
                ttl_seconds: None,
            };
            match ember_repo::create(pool, new).await {
                Ok(e) => println!("{} sent (ttl: {}s)", e.id, e.ttl_seconds),
                Err(e) => die(&format!("{e}")),
            }
        }
        "sweep" => match ember_repo::sweep_expired(pool).await {
            Ok(count) => println!("{count} expired embers cleaned up"),
            Err(e) => die(&format!("{e}")),
        },
        other => die(&format!("unknown ember subcommand '{other}'")),
    }
}

// ── Event ────────────────────────────────────────────

async fn handle_event(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() || args[0] == "list" || args[0] == "ls" {
        if args.len() < 2 {
            die("event list requires <spark_id>");
        }
        let spark_id = if args[0] == "list" || args[0] == "ls" {
            &args[1]
        } else {
            &args[0]
        };
        match event_repo::list_for_spark(pool, spark_id).await {
            Ok(events) => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&events).unwrap_or_default()
                    );
                } else if events.is_empty() {
                    println!("No events for {spark_id}.");
                } else {
                    for e in &events {
                        let old = e.old_value.as_deref().unwrap_or("null");
                        let new = e.new_value.as_deref().unwrap_or("null");
                        let actor_type = e.actor_type.as_deref().unwrap_or("");
                        println!(
                            "[{}] {} ({}): {} {} -> {}",
                            e.timestamp, e.actor, actor_type, e.field_name, old, new
                        );
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        }
    }
}

// ── Assignment ───────────────────────────────────────

async fn handle_assignment(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() {
        die("assign subcommand required (claim, release, list)");
    }
    match args[0].as_str() {
        "claim" => {
            if args.len() < 3 {
                die("assign claim requires <session_id> <spark_id>");
            }
            let new = NewHandAssignment {
                session_id: args[1].clone(),
                spark_id: args[2].clone(),
                role: AssignmentRole::Owner,
            };
            match assignment_repo::assign(pool, new).await {
                Ok(a) => println!("{} claimed by {} ({})", a.spark_id, a.session_id, a.role),
                Err(e) => die(&format!("{e}")),
            }
        }
        "release" => {
            if args.len() < 3 {
                die("assign release requires <session_id> <spark_id>");
            }
            match assignment_repo::abandon(pool, &args[1], &args[2]).await {
                Ok(()) => println!("{} released by {}", args[2], args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            if args.len() < 2 {
                die("assign list requires <spark_id>");
            }
            match assignment_repo::active_for_spark(pool, &args[1]).await {
                Ok(Some(a)) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&a).unwrap_or_default());
                    } else {
                        println!(
                            "{} owned by {} (since {})",
                            a.spark_id, a.session_id, a.assigned_at
                        );
                    }
                }
                Ok(None) => println!("{} is unclaimed.", args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown assign subcommand '{other}'")),
    }
}

// ── Commit ───────────────────────────────────────────

async fn handle_commit(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() {
        die("commit subcommand required (link, list, scan)");
    }
    match args[0].as_str() {
        "link" => {
            if args.len() < 3 {
                die("commit link requires <spark_id> <hash>");
            }
            let new = NewCommitLink {
                spark_id: args[1].clone(),
                commit_hash: args[2].clone(),
                commit_message: None,
                author: None,
                committed_at: None,
                workshop_id: ws_id.to_string(),
                linked_by: "cli".to_string(),
            };
            match commit_link_repo::create(pool, new).await {
                Ok(c) => println!("linked {} to {}", c.commit_hash, c.spark_id),
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            if args.len() < 2 {
                die("commit list requires <spark_id>");
            }
            match commit_link_repo::list_for_spark(pool, &args[1]).await {
                Ok(links) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&links).unwrap_or_default()
                        );
                    } else if links.is_empty() {
                        println!("No commits linked to {}.", args[1]);
                    } else {
                        for l in &links {
                            let msg = l.commit_message.as_deref().unwrap_or("");
                            println!(
                                "{} ({}) {}",
                                &l.commit_hash[..8.min(l.commit_hash.len())],
                                l.linked_by,
                                msg
                            );
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "scan" => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            match data::git::scan_commits_for_sparks(&cwd, None).await {
                Ok(refs) => {
                    if refs.is_empty() {
                        println!("No commits referencing sparks found.");
                    } else {
                        for r in &refs {
                            println!(
                                "{} [{}] {} — {}",
                                &r.hash[..8],
                                r.spark_ids.join(", "),
                                r.author,
                                r.message
                            );
                        }
                        // Auto-link discovered references
                        let mut linked = 0;
                        for r in &refs {
                            for sid in &r.spark_ids {
                                let new = NewCommitLink {
                                    spark_id: sid.clone(),
                                    commit_hash: r.hash.clone(),
                                    commit_message: Some(r.message.clone()),
                                    author: Some(r.author.clone()),
                                    committed_at: Some(r.timestamp.clone()),
                                    workshop_id: ws_id.to_string(),
                                    linked_by: "scan".to_string(),
                                };
                                if commit_link_repo::create(pool, new).await.is_ok() {
                                    linked += 1;
                                }
                            }
                        }
                        if linked > 0 {
                            println!("\n{linked} commit-spark links created.");
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown commit subcommand '{other}'")),
    }
}

// ── Parsers ──────────────────────────────────────────

fn parse_spark_type(s: &str) -> SparkType {
    match s {
        "bug" => SparkType::Bug,
        "feature" => SparkType::Feature,
        "task" => SparkType::Task,
        "epic" => SparkType::Epic,
        "chore" => SparkType::Chore,
        "spike" => SparkType::Spike,
        "milestone" => SparkType::Milestone,
        _ => {
            eprintln!("warning: unknown type '{s}', defaulting to task");
            SparkType::Task
        }
    }
}

fn parse_risk_level(s: &str) -> RiskLevel {
    match s {
        "trivial" => RiskLevel::Trivial,
        "normal" => RiskLevel::Normal,
        "elevated" => RiskLevel::Elevated,
        "critical" => RiskLevel::Critical,
        _ => {
            eprintln!("warning: unknown risk '{s}', defaulting to normal");
            RiskLevel::Normal
        }
    }
}

fn parse_bond_type(s: &str) -> BondType {
    match s {
        "blocks" => BondType::Blocks,
        "parent_child" => BondType::ParentChild,
        "related" => BondType::Related,
        "conditional_blocks" => BondType::ConditionalBlocks,
        "waits_for" => BondType::WaitsFor,
        "duplicates" => BondType::Duplicates,
        "supersedes" => BondType::Supersedes,
        _ => die(&format!(
            "invalid bond type '{s}' (blocks, parent_child, related, conditional_blocks, waits_for, duplicates, supersedes)"
        )),
    }
}

fn parse_contract_kind(s: &str) -> ContractKind {
    match s {
        "test_pass" => ContractKind::TestPass,
        "no_api_break" => ContractKind::NoApiBreak,
        "custom_command" => ContractKind::CustomCommand,
        "grep_absent" => ContractKind::GrepAbsent,
        "grep_present" => ContractKind::GrepPresent,
        _ => die(&format!(
            "invalid contract kind '{s}' (test_pass, no_api_break, custom_command, grep_absent, grep_present)"
        )),
    }
}

// ── Crew ─────────────────────────────────────────────

async fn handle_crew(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() {
        die("crew subcommand required (create, list, show, add-member, remove-member, status)");
    }
    match args[0].as_str() {
        "create" => {
            // Parse: create [--purpose <t>] [--parent <id>] [--head-session <id>] <name words...>
            let mut purpose: Option<String> = None;
            let mut parent: Option<String> = None;
            let mut head_session: Option<String> = None;
            let mut name_parts: Vec<&str> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--purpose" => {
                        i += 1;
                        if i < args.len() {
                            purpose = Some(args[i].clone());
                        }
                    }
                    "--parent" => {
                        i += 1;
                        if i < args.len() {
                            parent = Some(args[i].clone());
                        }
                    }
                    "--head-session" => {
                        i += 1;
                        if i < args.len() {
                            head_session = Some(args[i].clone());
                        }
                    }
                    other => name_parts.push(other),
                }
                i += 1;
            }
            let name = name_parts.join(" ");
            if name.is_empty() {
                die("crew create requires a <name>");
            }
            let new = NewCrew {
                name,
                purpose,
                workshop_id: ws_id.to_string(),
                head_session_id: head_session,
                parent_spark_id: parent,
            };
            match crew_repo::create(pool, new).await {
                Ok(c) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&c).unwrap_or_default());
                    } else {
                        println!("created {} — {}", c.id, c.name);
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => match crew_repo::list_for_workshop(pool, ws_id).await {
            Ok(crews) => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&crews).unwrap_or_default()
                    );
                } else if crews.is_empty() {
                    println!("No crews in this workshop.");
                } else {
                    println!("{:<12} {:<10} {:<24} PURPOSE", "ID", "STATUS", "NAME");
                    let sep = "-".repeat(72);
                    println!("{sep}");
                    for c in &crews {
                        let purpose = c.purpose.as_deref().unwrap_or("");
                        println!("{:<12} {:<10} {:<24} {}", c.id, c.status, c.name, purpose);
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        },
        "show" => {
            if args.len() < 2 {
                die("crew show requires <crew_id>");
            }
            let crew = match crew_repo::get(pool, &args[1]).await {
                Ok(c) => c,
                Err(e) => die(&format!("{e}")),
            };
            let members = crew_repo::members(pool, &crew.id).await.unwrap_or_default();
            if json_mode {
                let payload = serde_json::json!({ "crew": crew, "members": members });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            } else {
                println!("ID:      {}", crew.id);
                println!("Name:    {}", crew.name);
                println!("Status:  {}", crew.status);
                if let Some(ref p) = crew.purpose {
                    println!("Purpose: {p}");
                }
                if let Some(ref h) = crew.head_session_id {
                    println!("Head:    {h}");
                }
                if let Some(ref s) = crew.parent_spark_id {
                    println!("Parent:  {s}");
                }
                println!("Created: {}", crew.created_at);
                println!();
                if members.is_empty() {
                    println!("No members.");
                } else {
                    println!("Members:");
                    for m in &members {
                        let role = m.role.as_deref().unwrap_or("hand");
                        println!("  {} ({}) joined {}", m.session_id, role, m.joined_at);
                    }
                }
            }
        }
        "add-member" => {
            if args.len() < 3 {
                die("crew add-member requires <crew_id> <session_id> [--role hand|merger]");
            }
            let mut role: Option<String> = None;
            let mut i = 3;
            while i < args.len() {
                if args[i] == "--role" {
                    i += 1;
                    if i < args.len() {
                        role = Some(args[i].clone());
                    }
                }
                i += 1;
            }
            match crew_repo::add_member(pool, &args[1], &args[2], role.as_deref()).await {
                Ok(m) => println!(
                    "added {} to crew {} ({})",
                    m.session_id,
                    m.crew_id,
                    m.role.as_deref().unwrap_or("hand")
                ),
                Err(e) => die(&format!("{e}")),
            }
        }
        "remove-member" => {
            if args.len() < 3 {
                die("crew remove-member requires <crew_id> <session_id>");
            }
            match crew_repo::remove_member(pool, &args[1], &args[2]).await {
                Ok(()) => println!("removed {} from crew {}", args[2], args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "status" => {
            if args.len() < 3 {
                die("crew status requires <crew_id> <active|merging|completed|abandoned>");
            }
            if CrewStatus::from_str(&args[2]).is_none() {
                die(&format!(
                    "invalid crew status '{}' (active|merging|completed|abandoned)",
                    args[2]
                ));
            }
            match crew_repo::set_status(pool, &args[1], &args[2]).await {
                Ok(()) => println!("crew {} -> {}", args[1], args[2]),
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown crew subcommand '{other}'")),
    }
}

// ── Hand ─────────────────────────────────────────────

async fn handle_hand(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    args: &[String],
    json_mode: bool,
) {
    if args.is_empty() {
        die("hand subcommand required (spawn, list). Try `ryve hand --help`.");
    }
    if matches!(args[0].as_str(), "help" | "--help" | "-h") {
        print_hand_usage();
        return;
    }
    match args[0].as_str() {
        "spawn" => {
            if args.len() >= 2 && matches!(args[1].as_str(), "help" | "--help" | "-h") {
                print_hand_spawn_usage();
                return;
            }
            if args.len() < 2 {
                die(
                    "hand spawn requires <spark_id> [--agent <name>] [--role owner|head|merger] [--crew <id>]. Try `ryve hand spawn --help`.",
                );
            }
            let spark_id = args[1].clone();
            let mut agent_name: Option<String> = None;
            let mut role = HandKind::Owner;
            let mut crew_id: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--agent" => {
                        i += 1;
                        if i < args.len() {
                            agent_name = Some(args[i].clone());
                        }
                    }
                    "--role" => {
                        i += 1;
                        if i < args.len() {
                            role = match args[i].as_str() {
                                "owner" | "hand" => HandKind::Owner,
                                "head" => HandKind::Head,
                                "merger" => HandKind::Merger,
                                other => {
                                    die(&format!("invalid role '{other}' (owner|head|merger)"))
                                }
                            };
                        }
                    }
                    "--crew" => {
                        i += 1;
                        if i < args.len() {
                            crew_id = Some(args[i].clone());
                        }
                    }
                    other => die(&format!("unknown hand spawn flag '{other}'")),
                }
                i += 1;
            }

            let agent = resolve_agent(agent_name.as_deref());

            // The spawning Hand (typically a Head) had its own session id
            // injected into env at spawn time as `RYVE_HAND_SESSION_ID`.
            // Pass it through so the new row's `parent_session_id` records
            // the lineage. Direct CLI use by a human will simply have no
            // env var set and the column will be NULL.
            let parent_session_id = std::env::var("RYVE_HAND_SESSION_ID").ok();

            match hand_spawn::spawn_hand(
                workshop_root,
                pool,
                &agent,
                &spark_id,
                role,
                crew_id.as_deref(),
                parent_session_id.as_deref(),
            )
            .await
            {
                Ok(spawned) => {
                    if json_mode {
                        let payload = serde_json::json!({
                            "session_id": spawned.session_id,
                            "spark_id": spawned.spark_id,
                            "worktree": spawned.worktree_path,
                            "log": spawned.log_path,
                            "pid": spawned.child_pid,
                        });
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&payload).unwrap_or_default()
                        );
                    } else {
                        println!(
                            "spawned hand {} on spark {} (pid {:?})",
                            spawned.session_id, spawned.spark_id, spawned.child_pid
                        );
                        println!("  worktree: {}", spawned.worktree_path.display());
                        println!("  log:      {}", spawned.log_path.display());
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => match assignment_repo::list_active(pool).await {
            Ok(active) => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&active).unwrap_or_default()
                    );
                } else if active.is_empty() {
                    println!("No active hand assignments.");
                } else {
                    println!("{:<12} {:<36} {:<10} ASSIGNED", "SPARK", "SESSION", "ROLE");
                    let sep = "-".repeat(80);
                    println!("{sep}");
                    for a in &active {
                        println!(
                            "{:<12} {:<36} {:<10} {}",
                            a.spark_id, a.session_id, a.role, a.assigned_at
                        );
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        },
        other => die(&format!(
            "unknown hand subcommand '{other}'. Try `ryve hand --help`."
        )),
    }
}

// ── Hand / Head help text ─────────────────────────────

fn print_hand_usage() {
    println!(
        "ryve hand — spawn and inspect Hand coding-agent subprocesses

A **Hand** is a coding-agent subprocess (claude, codex, aider, opencode, …)
that owns a single spark and executes its work inside an isolated git
worktree under `.ryve/worktrees/<short>/` on branch `hand/<short>`. Hands
are the only layer that edits code.

USAGE:
  ryve hand spawn <spark_id> [--agent <name>] [--role owner|head|merger] [--crew <id>]
  ryve hand list
  ryve hand --help
  ryve hand spawn --help

SUBCOMMANDS:
  spawn    Launch a detached Hand subprocess on a spark.
  list     Show active hand assignments (ownership + role + heartbeat).

ROLES:
  owner    (default) Standard worker. Claims the spark, works in its own
           worktree, closes the spark when DONE.md passes.
  head     Crew orchestrator. Takes an epic, decomposes it into child
           sparks, spawns sub-Hands, and supervises them. Prefer the
           dedicated `ryve head spawn` wrapper — it reads more naturally.
  merger   Crew integrator. Collects sibling worktrees, merges them into a
           single `crew/<id>` branch, and opens one PR. Requires --crew.

See also:
  ryve head --help                    Head-specific documentation.
  docs/AGENT_HIERARCHY.md              Atlas → Head → Hand overview.
  docs/HAND_CAPABILITIES.md            Hand capability classes.
"
    );
}

fn print_hand_spawn_usage() {
    println!(
        "ryve hand spawn — launch a Hand subprocess on a spark

USAGE:
  ryve hand spawn <spark_id> [OPTIONS]

ARGUMENTS:
  <spark_id>   The spark the new Hand will own. For `--role head` this is
               the parent epic the Head will decompose.

OPTIONS:
  --agent <name>           Coding agent to run (claude, codex, aider,
                           opencode, …). Defaults to the first detected
                           agent on your PATH.
  --role <role>            owner | head | merger. Default: owner.
                             owner  — standard worker Hand.
                             head   — crew orchestrator (prefer `ryve head spawn`).
                             merger — crew integrator (requires --crew).
  --crew <crew_id>         Attach the new Hand to an existing crew as a
                           member. Required when --role merger.

EFFECTS:
  1. Creates a git worktree at `.ryve/worktrees/<short>/` on branch
     `hand/<short>`.
  2. Persists an `agent_sessions` row (session_label = hand / head /
     merger) and a `hand_assignments` row claiming the spark.
  3. If --crew is set, inserts a `crew_members` row.
  4. Writes a role-specific initial prompt under `.ryve/prompts/` and
     launches the chosen coding agent in full-auto mode, detached, with
     stdout/stderr redirected to `.ryve/logs/hand-<session>.log`.

The new subprocess survives the `ryve hand spawn` exit — the UI picks it
up on the next workgraph poll (~3 s).

EXAMPLES:
  # Spawn a standard worker Hand on spark sp-1234 using claude:
  ryve hand spawn sp-1234 --agent claude

  # Spawn a merger for crew cr-abcd:
  ryve hand spawn sp-merge1 --role merger --crew cr-abcd --agent claude
"
    );
}

fn print_head_usage() {
    println!(
        "ryve head — spawn and inspect Head crew orchestrators

A **Head** is a coding-agent subprocess that orchestrates a **Crew** of
Hands working in parallel on related sparks. Mechanically a Head is the
same kind of subprocess as a Hand (same worktree machinery, same session
row, same launch flow); what distinguishes it is the *system prompt* and
the *role label* (`agent_sessions.session_label = \"head\"`).

A Head's job is to:
  1. Read the parent epic and its acceptance criteria.
  2. Decompose the epic into 2–8 child task sparks.
  3. Create a Crew bundling those sparks together.
  4. Spawn one Hand per child spark (via `ryve hand spawn --crew <id>`).
  5. Poll progress; reassign stalled Hands.
  6. When every child is closed, spawn a Merger Hand to integrate the
     worktrees into one PR.
  7. Post the resulting PR URL on the parent epic and exit.

A Head NEVER edits code itself — if a Head finds itself wanting to write
a patch, it must spawn a Hand on a spark instead.

LIFECYCLE:
  Atlas (Director) → Head → Crew → {{ Hand, Hand, …, Merger }}

ARCHETYPES:
  Heads come in three standard archetypes (documented in
  `docs/HEAD_ARCHETYPES.md`):
    build     Ship code that satisfies acceptance criteria.
    research  Reduce uncertainty before code is written.
    review    Critique existing code, designs, or PRs.
  Archetypes are a prompting/delegation contract, not a new subprocess
  type. To add a new archetype see `docs/HEAD_HOWTO.md`.

USAGE:
  ryve head spawn <epic_id> [--agent <name>] [--crew <id>]
  ryve head list
  ryve head --help
  ryve head spawn --help

SUBCOMMANDS:
  spawn    Launch a Head subprocess on an epic spark.
  list     Show active Head sessions (session_label = head).

See also:
  docs/AGENT_HIERARCHY.md     Full Atlas → Head → Hand hierarchy.
  docs/HEAD_ARCHETYPES.md     The three standard Head archetypes.
  docs/HEAD_PLAN.md           Implementation plan and rationale.
  docs/HEAD_HOWTO.md          How to add a new archetype.
  .ryve/WORKSHOP.md           Heads section + worked example.
"
    );
}

fn print_head_spawn_usage() {
    println!(
        "ryve head spawn — launch a Head (crew orchestrator) on an epic

USAGE:
  ryve head spawn <epic_id> [OPTIONS]

ARGUMENTS:
  <epic_id>    The parent epic spark the Head will decompose. Should have
               a populated `problem_statement` and `acceptance_criteria`
               intent (`ryve spark show <epic_id>` to verify before
               spawning).

OPTIONS:
  --agent <name>   Coding agent to run as the Head (claude, codex, aider,
                   opencode, …). Defaults to the first detected agent.
  --crew <id>      Attach the Head to an existing crew. Optional: most
                   workflows create the crew from inside the Head itself
                   via `ryve crew create --head-session $RYVE_SESSION_ID`.

EFFECTS:
  1. Creates a git worktree for the Head at `.ryve/worktrees/<short>/`.
  2. Persists an `agent_sessions` row with `session_label = \"head\"`.
  3. Persists a `hand_assignments` row claiming the epic with role Owner
     (the Head \"owns\" the epic for the lifetime of its crew).
  4. Writes the Head system prompt (composed via `compose_head_prompt`,
     injecting the epic id + title) to `.ryve/prompts/hand-<id>.md`.
  5. Launches the agent in full-auto, detached, with stdout/stderr going
     to `.ryve/logs/hand-<id>.log`.

WORKED EXAMPLE:
  # 1. Create an epic capturing the goal:
  ryve spark create --type epic --priority 1 \\
      --problem 'add OAuth login to the dashboard' \\
      --acceptance 'user can log in with Google on /login' \\
      --acceptance 'session cookie set and verified on /dashboard' \\
      'Add OAuth login'
  # → prints something like `created sp-1234`.

  # 2. Spawn a Head on that epic:
  ryve head spawn sp-1234 --agent claude

  # 3. Watch the Head work. It will:
  #    - create 2–8 child sparks under sp-1234
  #    - create a crew and spawn Hands on each child
  #    - poll progress
  #    - eventually spawn a Merger that opens a single PR
  #    - post the PR URL as a comment on sp-1234

  # 4. Observe:
  ryve spark show sp-1234    # children appear bonded parent_child
  ryve crew list             # crew appears
  ryve hand list             # sub-Hands appear with role=owner
  ryve head list             # the Head itself appears
"
    );
}

// ── Head command dispatch ─────────────────────────────

/// `ryve head` — spawn and inspect Head crew orchestrators.
///
/// A Head is mechanically a Hand (same worktree, same session machinery),
/// distinguished by `session_label = \"head\"` and the Head system prompt.
/// `ryve head spawn` delegates to the same `hand_spawn::spawn_hand`
/// helper as `ryve hand spawn`, just with `HandKind::Head`.
async fn handle_head(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    args: &[String],
    json_mode: bool,
) {
    if args.is_empty() {
        print_head_usage();
        return;
    }
    if matches!(args[0].as_str(), "help" | "--help" | "-h") {
        print_head_usage();
        return;
    }
    match args[0].as_str() {
        "spawn" => {
            if args.len() >= 2 && matches!(args[1].as_str(), "help" | "--help" | "-h") {
                print_head_spawn_usage();
                return;
            }
            if args.len() < 2 {
                die(
                    "head spawn requires <epic_id> [--agent <name>] [--crew <id>]. Try `ryve head spawn --help`.",
                );
            }
            let epic_id = args[1].clone();
            let mut agent_name: Option<String> = None;
            let mut crew_id: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--agent" => {
                        i += 1;
                        if i < args.len() {
                            agent_name = Some(args[i].clone());
                        }
                    }
                    "--crew" => {
                        i += 1;
                        if i < args.len() {
                            crew_id = Some(args[i].clone());
                        }
                    }
                    other => die(&format!(
                        "unknown head spawn flag '{other}'. Try `ryve head spawn --help`."
                    )),
                }
                i += 1;
            }

            let agent = resolve_agent(agent_name.as_deref());
            let parent_session_id = std::env::var("RYVE_HAND_SESSION_ID").ok();

            match hand_spawn::spawn_hand(
                workshop_root,
                pool,
                &agent,
                &epic_id,
                HandKind::Head,
                crew_id.as_deref(),
                parent_session_id.as_deref(),
            )
            .await
            {
                Ok(spawned) => {
                    if json_mode {
                        let payload = serde_json::json!({
                            "session_id": spawned.session_id,
                            "epic_id": spawned.spark_id,
                            "worktree": spawned.worktree_path,
                            "log": spawned.log_path,
                            "pid": spawned.child_pid,
                            "role": "head",
                        });
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&payload).unwrap_or_default()
                        );
                    } else {
                        println!(
                            "spawned head {} on epic {} (pid {:?})",
                            spawned.session_id, spawned.spark_id, spawned.child_pid
                        );
                        println!("  worktree: {}", spawned.worktree_path.display());
                        println!("  log:      {}", spawned.log_path.display());
                        println!(
                            "  tip: `ryve head list` to see it, `ryve crew list` once it creates a crew"
                        );
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            // A "Head" is an agent_sessions row with session_label = "head".
            // We reuse the existing list_active helper and filter, but
            // there is no shared helper for session_label filtering — walk
            // active assignments and cross-reference the session label.
            let ws_id = workshop_root
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let sessions = match agent_session_repo::list_for_workshop(pool, &ws_id).await {
                Ok(s) => s,
                Err(e) => die(&format!("{e}")),
            };
            let heads: Vec<_> = sessions
                .iter()
                .filter(|s| s.session_label.as_deref() == Some("head") && s.status == "active")
                .collect();
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&heads).unwrap_or_default()
                );
            } else if heads.is_empty() {
                println!("No active Head sessions.");
            } else {
                println!("{:<36} {:<16} STARTED", "SESSION", "AGENT");
                let sep = "-".repeat(80);
                println!("{sep}");
                for h in &heads {
                    println!("{:<36} {:<16} {}", h.id, h.agent_name, h.started_at);
                }
            }
        }
        other => die(&format!(
            "unknown head subcommand '{other}'. Try `ryve head --help`."
        )),
    }
}

// ── Worktree pruning ─────────────────────────────────

/// `ryve worktree prune` (alias `ryve wt prune`).
///
/// Walks `.ryve/worktrees/<short_id>/` and classifies each one via the
/// pure [`worktree_cleanup::classify_worktree`] predicate. Dry-run by
/// default — prints a per-row report and a summary. Pass `--yes` to
/// actually run `git worktree remove --force` and `git branch -D
/// hand/<short_id>` for every Removable candidate.
///
/// Spark `ryve-261d06f3` (Layer A of epic `ryve-b61e7ed4`).
async fn handle_worktree(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    args: &[String],
    json_mode: bool,
) {
    if args.is_empty() {
        die("worktree subcommand required (prune)");
    }
    match args[0].as_str() {
        "prune" => handle_worktree_prune(pool, workshop_root, &args[1..], json_mode).await,
        other => die(&format!("unknown worktree subcommand '{other}'")),
    }
}

async fn handle_worktree_prune(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    args: &[String],
    json_mode: bool,
) {
    // Parse flags. The only flag for now is --yes; everything else is
    // an error so a typo'd `--all` doesn't silently nuke worktrees.
    let mut apply = false;
    for arg in args {
        match arg.as_str() {
            "--yes" | "-y" => apply = true,
            other => die(&format!("unknown worktree prune flag '{other}'")),
        }
    }

    // Gather facts for every directory under .ryve/worktrees/. We need
    // the live agent_sessions snapshot to answer "is this hand still
    // active?" — pull it once up front.
    let workshop_id = workshop_root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let sessions = agent_session_repo::list_for_workshop(pool, &workshop_id)
        .await
        .unwrap_or_default();

    let worktrees_dir = workshop_root.join(".ryve").join("worktrees");
    if !worktrees_dir.exists() {
        if json_mode {
            println!(
                "{{\"candidates\":[],\"summary\":{{\"removable\":0,\"dirty\":0,\"unmerged\":0,\"live\":0,\"out_of_scope\":0}}}}"
            );
        } else {
            println!("no .ryve/worktrees/ directory — nothing to prune");
        }
        return;
    }

    let entries = match std::fs::read_dir(&worktrees_dir) {
        Ok(e) => e,
        Err(e) => die(&format!("read .ryve/worktrees/: {e}")),
    };

    let mut candidates: Vec<PruneCandidate> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        let facts = gather_worktree_facts(&path, &dir_name, workshop_root, &sessions);
        let status = classify_worktree(&facts);
        candidates.push(PruneCandidate { facts, status });
    }

    // Sort: Removable first (so the user sees actionable items at the
    // top), then UnmergedCommits, then Dirty, then Live, then
    // out-of-scope. Within a status group, alphabetical by short_id so
    // the report is deterministic.
    candidates.sort_by(|a, b| {
        let rank = |s: &WorktreeStatus| match s {
            WorktreeStatus::Removable => 0,
            WorktreeStatus::UnmergedCommits(_) => 1,
            WorktreeStatus::DirtyTree => 2,
            WorktreeStatus::LiveSession => 3,
            WorktreeStatus::NotHandWorktree => 4,
        };
        rank(&a.status)
            .cmp(&rank(&b.status))
            .then_with(|| a.facts.short_id.cmp(&b.facts.short_id))
    });

    let mut summary = PruneSummary::default();
    for c in &candidates {
        summary.record(&c.status);
    }

    if json_mode {
        print_prune_report_json(&candidates, &summary);
    } else {
        print_prune_report_text(&candidates, &summary, apply);
    }

    if !apply {
        return;
    }

    // Apply path: remove every Removable candidate. We do NOT touch
    // anything in any other status — the predicate already gated this.
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    for c in &candidates {
        if !matches!(c.status, WorktreeStatus::Removable) {
            continue;
        }
        let short_id = c.facts.short_id.as_deref().unwrap_or("?");
        let branch = c
            .facts
            .branch
            .clone()
            .unwrap_or_else(|| format!("hand/{short_id}"));

        match worktree_cleanup::run_worktree_remove(workshop_root, &c.facts.path) {
            Ok(()) => {
                if let Err(e) = worktree_cleanup::run_branch_delete(workshop_root, &branch) {
                    eprintln!("  ! {short_id} worktree removed but branch delete failed: {e}");
                    // Count as success — the worktree is gone, the
                    // branch is just a stale ref the user can delete
                    // separately.
                }
                println!("  ✓ removed {short_id} ({branch})");
                succeeded += 1;
            }
            Err(e) => {
                eprintln!("  ✗ {short_id}: {e}");
                failed += 1;
            }
        }
    }

    println!();
    if failed == 0 {
        println!("removed {succeeded} worktree{}", plural(succeeded));
    } else {
        println!("removed {succeeded}, failed {failed} (see errors above)",);
    }
}

/// Build a [`WorktreeFacts`] for one directory under `.ryve/worktrees/`.
/// This is the side-effect-y counterpart to the pure
/// [`classify_worktree`] — it shells out to git and queries the live
/// `agent_sessions` snapshot. Kept here (not in `worktree_cleanup`) so
/// the cleanup module stays test-friendly.
fn gather_worktree_facts(
    path: &Path,
    dir_name: &str,
    workshop_root: &Path,
    sessions: &[PersistedAgentSession],
) -> WorktreeFacts {
    let short_id = worktree_cleanup::parse_short_id(dir_name);
    let branch = worktree_cleanup::worktree_branch(path);
    let is_clean = worktree_cleanup::worktree_is_clean(path);

    // Unmerged count is checked against the workshop's main repo, not
    // the worktree itself — `git rev-list --count main..hand/<id>` from
    // the workshop root resolves the same refs.
    let unmerged_count = match branch.as_deref() {
        Some(b) => worktree_cleanup::unmerged_count(workshop_root, b),
        None => u32::MAX, // unknown branch → don't auto-remove
    };

    // Live session check: a session is "live" if its row matches this
    // short_id (by id prefix) AND either status='active' or its
    // child_pid is still alive. We match on the first 8 chars of the
    // session id since that's how worktree dirs are named.
    let session_live = if let Some(sid) = short_id.as_deref() {
        sessions.iter().any(|s| {
            if !s.id.starts_with(sid) {
                return false;
            }
            if s.status == "active" {
                return true;
            }
            // status='ended' but child_pid alive can happen if the row
            // was force-ended but the agent didn't actually exit.
            s.child_pid
                .and_then(|pid| u32::try_from(pid).ok())
                .map(worktree_cleanup::process_is_alive)
                .unwrap_or(false)
        })
    } else {
        false
    };

    WorktreeFacts {
        path: path.to_path_buf(),
        short_id,
        branch,
        is_clean,
        unmerged_count,
        session_live,
    }
}

fn print_prune_report_text(candidates: &[PruneCandidate], summary: &PruneSummary, apply: bool) {
    if candidates.is_empty() {
        println!("no worktrees under .ryve/worktrees/");
        return;
    }

    println!(
        "{} worktree{} found under .ryve/worktrees/",
        candidates.len(),
        plural(candidates.len())
    );
    println!();

    for c in candidates {
        let id = c.facts.short_id.as_deref().unwrap_or("(non-hand)");
        let branch = c.facts.branch.as_deref().unwrap_or("(no branch)");
        println!(
            "  {} {:8}  {:24}  {}",
            c.status.glyph(),
            id,
            branch,
            c.status.reason()
        );
    }

    println!();
    println!(
        "summary: {} removable, {} unmerged, {} dirty, {} live, {} out-of-scope",
        summary.removable, summary.unmerged, summary.dirty, summary.live, summary.out_of_scope,
    );

    if !apply {
        if summary.removable > 0 {
            println!();
            println!(
                "dry-run: pass --yes to remove the {} removable worktree{}",
                summary.removable,
                plural(summary.removable),
            );
        } else {
            println!();
            println!("dry-run: nothing to remove");
        }
    }
}

fn print_prune_report_json(candidates: &[PruneCandidate], summary: &PruneSummary) {
    let mut items: Vec<serde_json::Value> = Vec::with_capacity(candidates.len());
    for c in candidates {
        items.push(serde_json::json!({
            "short_id": c.facts.short_id,
            "branch": c.facts.branch,
            "path": c.facts.path,
            "is_clean": c.facts.is_clean,
            "unmerged_count": c.facts.unmerged_count,
            "session_live": c.facts.session_live,
            "status": match &c.status {
                WorktreeStatus::Removable => "removable",
                WorktreeStatus::DirtyTree => "dirty",
                WorktreeStatus::UnmergedCommits(_) => "unmerged",
                WorktreeStatus::LiveSession => "live",
                WorktreeStatus::NotHandWorktree => "out_of_scope",
            },
        }));
    }
    let payload = serde_json::json!({
        "candidates": items,
        "summary": {
            "removable": summary.removable,
            "dirty": summary.dirty,
            "unmerged": summary.unmerged,
            "live": summary.live,
            "out_of_scope": summary.out_of_scope,
        },
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Resolve a coding-agent name (or `None`) to a `CodingAgent` definition.
/// When the name matches a known agent (`claude`, `codex`, etc.) we use the
/// canonical definition; otherwise we fall back to a minimal stub so users
/// can pass arbitrary commands like `echo` for testing.
fn resolve_agent(name: Option<&str>) -> CodingAgent {
    let name = name.unwrap_or("claude");
    if let Some(known) = coding_agents::known_agents()
        .into_iter()
        .find(|a| a.command == name || a.display_name.eq_ignore_ascii_case(name))
    {
        return known;
    }
    // Fallback: build a stub agent for this command. No system-prompt flag,
    // no resume support — used by tests and for one-off custom commands.
    CodingAgent {
        display_name: name.to_string(),
        command: name.to_string(),
        args: Vec::new(),
        resume: crate::coding_agents::ResumeStrategy::None,
        compatibility: crate::coding_agents::CompatStatus::Unknown,
    }
}
