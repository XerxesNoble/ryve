// SPDX-License-Identifier: AGPL-3.0-or-later

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
use data::sparks::error::SparksError;
use data::sparks::types::*;
use data::sparks::{
    agent_session_repo, assignment_repo, bond_repo, comment_repo, commit_link_repo,
    constraint_helpers, contract_repo, crew_repo, ember_repo, event_repo, release_repo, spark_repo,
    stamp_repo, watch_repo,
};

use crate::agent_prompts::HeadArchetype;
use crate::coding_agents::{self, CodingAgent};
use crate::hand_archetypes::{self, Action as PolicyAction, CallerArchetype};
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
    "release",
    "releases",
    "worktree",
    "worktrees",
    "wt",
    "tmux",
    "sweep",
    "watch",
    "watches",
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
        "head" | "heads" => {
            handle_head(&pool, &workshop_root, &ws_id, &args_clean[2..], json_mode).await
        }
        "release" | "releases" => {
            handle_release(&pool, &workshop_root, &args_clean[2..], json_mode).await
        }
        "worktree" | "worktrees" | "wt" => {
            handle_worktree(&pool, &workshop_root, &args_clean[2..], json_mode).await
        }
        "tmux" => handle_tmux(&pool, &workshop_root, &ws_id, &args_clean[2..], json_mode).await,
        "sweep" => handle_sweep(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "watch" | "watches" => handle_watch(&pool, &args_clean[2..], json_mode).await,
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

/// Resolve the archetype of the caller invoking this CLI command.
///
/// Every Hand subprocess Ryve spawns is handed `RYVE_HAND_SESSION_ID`
/// in its env so nested `ryve ...` invocations know who they are. This
/// helper looks that session up in `agent_sessions` and reads its
/// `session_label`, then maps the label to a
/// [`CallerArchetype`] via
/// [`hand_archetypes::caller_archetype_for_label`]. The result drives
/// the Release Manager allow-list in
/// [`hand_archetypes::enforce_action`] — spark ryve-e6713ee7
/// [sp-2a82fee7].
///
/// Returns [`CallerArchetype::Unrestricted`] for direct-human CLI use
/// (no env), for sessions missing from the DB (worktree reused across
/// workshops, stale env), and for any label not in the restricted
/// archetype set. Restricting by default would break every existing
/// human-driven workflow, so the policy is additive.
async fn current_caller_archetype(pool: &sqlx::SqlitePool) -> CallerArchetype {
    let Ok(session_id) = std::env::var("RYVE_HAND_SESSION_ID") else {
        return CallerArchetype::Unrestricted;
    };
    let label = match agent_session_repo::get(pool, &session_id).await {
        Ok(Some(row)) => row.session_label,
        _ => None,
    };
    hand_archetypes::caller_archetype_for_label(label.as_deref())
}

/// Enforce the caller's archetype against a candidate action and die
/// with a non-zero exit on any violation. The CLI-side complement of
/// the pure [`hand_archetypes::enforce_action`] check, wired into every
/// mutation entry point the Release Manager archetype restricts.
fn enforce_or_die(caller: CallerArchetype, action: &PolicyAction<'_>) {
    if let Err(e) = hand_archetypes::enforce_action(caller, action) {
        die(&format!("{e}"));
    }
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
    eprintln!("  spark create --parent <epic-id> <title>   Create a task spark under an epic");
    eprintln!("  spark create --type epic <title>          Create a top-level epic");
    eprintln!(
        "  spark create --help                       Show all create flags (intent, risk, etc.)"
    );
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
    eprintln!(
        "  assign override <session_id> <spark_id> --to in_progress --reason <text>  \
         Recover a stuck assignment (Head/Director only)"
    );
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
    eprintln!(
        "  hand spawn <spark_id> [--agent <name>] [--role owner|head|investigator|architect|reviewer|release_manager|bug_hunter|performance_engineer|merger] [--crew <id>]"
    );
    eprintln!("                                       Spawn a Hand subprocess on a spark");
    eprintln!("  hand list                            List active hand assignments");
    eprintln!();
    eprintln!("  head spawn <epic_id> --archetype <build|research|review> \\");
    eprintln!("            [--agent <name>] [--crew <id>]");
    eprintln!("                                       Spawn a Head subprocess on an epic");
    eprintln!("  head orchestrate <parent_spark> --children <csv>");
    eprintln!("      [--merge-spark <id>] [--crew-name <name>] [--purpose <text>]");
    eprintln!("      [--agent <name>] [--stall-seconds N] [--poll-seconds M] [--max-cycles N]");
    eprintln!("                                       Drive a Head→Crew loop via");
    eprintln!("                                       the shared orchestration module");
    eprintln!("  head list                            List active Head sessions");
    eprintln!("  head archetype list                  List registered Head archetypes");
    eprintln!("  head render <archetype> --epic <id>  Dry-run: render an archetype prompt");
    eprintln!("  head --help                          Long-form Head documentation");
    eprintln!();
    eprintln!("  release create <major|minor|patch>   Create a new release");
    eprintln!("  release list                         List releases");
    eprintln!("  release show <id>                    Show release details + member epics");
    eprintln!(
        "  release edit <id> [--version <semver>] [--notes <text>] [--clear-notes] [--problem <text>] [--clear-problem] [--branch <name>] [--clear-branch]"
    );
    eprintln!("      Update release fields in place (pass --clear-* to null a field)");
    eprintln!("  release add-epic <id> <epic_id>      Add an epic to a release");
    eprintln!("  release remove-epic <id> <epic_id>   Remove an epic from a release");
    eprintln!("  release status <id> <status>         Transition release status");
    eprintln!(
        "  release close <id>                   Close a release (verify, tag, build, record)"
    );
    eprintln!();
    eprintln!(
        "  worktree prune [--yes]               Prune stale hand worktrees (dry-run by default)"
    );
    eprintln!("  wt prune                             Alias for worktree prune");
    eprintln!();
    eprintln!(
        "  sweep stalls [--abandon]             Surface orphaned in_progress sparks (owner session ended)"
    );
    eprintln!();
    eprintln!(
        "  watch create <target_spark_id> --cadence <secs|cron:EXPR> \\\n      [--stop-condition <spec>] --intent <label>"
    );
    eprintln!("                                       Create a recurring watch");
    eprintln!("  watch list [--target <id>] [--status active|completed|cancelled]  List watches");
    eprintln!("  watch show <watch_id>                Show a watch's details");
    eprintln!("  watch cancel <watch_id>              Soft-cancel a watch");
    eprintln!(
        "  watch replace <watch_id> --cadence <c> [--stop-condition <s>] [--intent <l>]\n      Atomically cancel and recreate a watch"
    );
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
                &data::backup::RetentionPolicy::default(),
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
                .and_then(|v| v.parse::<usize>().ok());
            let policy = match keep {
                Some(k) => data::backup::RetentionPolicy {
                    keep_recent: k,
                    ..Default::default()
                },
                None => data::backup::RetentionPolicy::default(),
            };
            if !json_mode && let Some(k) = keep {
                eprintln!(
                    "note: --keep={k} sets only the recent-snapshot retention floor; default daily/weekly retention still applies, so more than {k} snapshot(s) may be kept"
                );
            }
            match data::backup::apply_retention(&ryve_dir, &policy).await {
                Ok(deleted) => {
                    if json_mode {
                        let json: Vec<_> =
                            deleted.iter().map(|p| p.display().to_string()).collect();
                        println!("{}", serde_json::to_string(&json).unwrap_or_default());
                    } else {
                        println!("pruned {} snapshot(s)", deleted.len());
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
                "  backup prune [--keep=N]      Prune old snapshots (default keep_recent={})",
                data::backup::KEEP_RECENT
            );
            eprintln!(
                "                               --keep=N only sets the recent-snapshot floor;"
            );
            eprintln!("                               daily/weekly taper retention still applies.");
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
                eprintln!(
                    "  --parent <spark-id>         Parent epic id. Required for any non-epic spark."
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
            let mut parent: Option<String> = None;
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
                    "--parent" => {
                        i += 1;
                        if i < args.len() {
                            parent = Some(args[i].clone());
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

            // Enforce the no-orphan rule at the CLI layer with a friendly message.
            // The data layer is the source of truth; this is a usability shim that
            // surfaces the rule before we hit a raw error from the repo.
            if spark_type != SparkType::Epic && parent.is_none() {
                die("non-epic spark requires a parent epic.\n  \
                     use --parent <epic-id> to nest under an existing epic, or\n  \
                     use --type epic if you intended a top-level spark.\n  \
                     run `ryve spark list --all` to find an epic id.");
            }

            // Validate the parent: must exist and be of type 'epic'.
            if let Some(ref pid) = parent {
                match spark_repo::get(pool, pid).await {
                    Ok(p) => {
                        if p.spark_type != SparkType::Epic.as_str() {
                            die(&format!(
                                "--parent {pid} resolves to a {} spark; only epics may be parents.",
                                p.spark_type
                            ));
                        }
                    }
                    Err(_) => die(&format!("--parent {pid}: spark not found")),
                }
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
                parent_id: parent.clone(),
                due_at: None,
                estimated_minutes: None,
                metadata,
                risk_level: risk,
                scope_boundary: scope,
            };
            match spark_repo::create(pool, new).await {
                Ok(spark) => {
                    // Mirror the parent linkage as a parent_child bond so the bond
                    // graph and the spark.parent_id column stay in sync.
                    if let Some(ref pid) = parent
                        && let Err(e) =
                            bond_repo::create(pool, pid, &spark.id, BondType::ParentChild).await
                    {
                        die(&format!(
                            "spark {} created but parent_child bond from {pid} failed: {e}",
                            spark.id
                        ));
                    }
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
            // Archetype policy gate (spark ryve-e6713ee7): the Release
            // Manager may only comment on sparks that are members of
            // some release — Atlas polls those; every other target is
            // outside the Atlas-only comms graph. Resolved with one
            // `SELECT 1 FROM release_epics` and combined with the
            // caller's archetype before the DB write.
            let caller = current_caller_archetype(pool).await;
            let is_release_spark = match release_repo::is_release_member(pool, &args[1]).await {
                Ok(v) => v,
                Err(e) => die(&format!("{e}")),
            };
            enforce_or_die(
                caller,
                &PolicyAction::CommentAdd {
                    spark_id: &args[1],
                    is_release_spark,
                },
            );
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
            // Archetype policy gate (spark ryve-e6713ee7): embers are
            // broadcasts and therefore outside the Release Manager's
            // Atlas-only comms graph.
            enforce_or_die(
                current_caller_archetype(pool).await,
                &PolicyAction::EmberSend,
            );
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

// ── Sweep (workgraph stall detectors) ────────────────
//
// [sp-312b98ad] Build orchestration invariant: when all child sparks of
// a build epic close, the merge/PR step must either complete, emit a
// visible blocked/failure signal, or remain owned by a live coordinator.
//
// The failure mode this sweep targets: a Build Head spawns the Merger
// via `finalize_with_merger` and exits. If the Merger subprocess later
// dies (crash, token exhaustion, manual kill) before it closes its
// merge spark, the `assignments` row is left `active` even though the
// owning `agent_sessions` row flips to `ended`. Nobody is polling the
// crew anymore, so the merge spark sits `in_progress` indefinitely —
// exactly the silent stall observed on ryve-e208c8ac (merge of epic
// ryve-18f4cec4). `sweep stalls` surfaces those orphans as flare
// embers so Atlas, the UI, and humans can see them.

async fn handle_sweep(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() {
        die("sweep subcommand required (stalls)");
    }
    match args[0].as_str() {
        "stalls" | "stall" => handle_sweep_stalls(pool, &args[1..], ws_id, json_mode).await,
        other => die(&format!("unknown sweep subcommand '{other}'")),
    }
}

async fn handle_sweep_stalls(
    pool: &sqlx::SqlitePool,
    args: &[String],
    ws_id: &str,
    json_mode: bool,
) {
    let mut abandon = false;
    for a in args {
        match a.as_str() {
            "--abandon" => abandon = true,
            other => die(&format!("unknown sweep stalls flag '{other}'")),
        }
    }

    let orphans = match assignment_repo::find_orphaned_claims(pool, ws_id).await {
        Ok(o) => o,
        Err(e) => die(&format!("find_orphaned_claims failed: {e}")),
    };

    // Dedupe embers per (spark, session) pair so repeated sweeps do not
    // spam the Ember bar. `find_recent_by_prefix` looks back far enough
    // to cover the default ember TTL (1 h).
    let mut emitted = Vec::with_capacity(orphans.len());
    for o in &orphans {
        let prefix = format!("stalled-claim {} ", o.spark_id);
        let dedupe =
            ember_repo::find_recent_by_prefix(pool, ws_id, EmberType::Flare, &prefix, 3600)
                .await
                .unwrap_or(None);
        if dedupe.is_some() {
            continue;
        }
        let content = format!(
            "stalled-claim {} role={} session={} ended_at={} title={}",
            o.spark_id,
            o.role,
            o.session_id,
            o.session_ended_at.as_deref().unwrap_or("?"),
            o.spark_title,
        );
        match ember_repo::create(
            pool,
            NewEmber {
                ember_type: EmberType::Flare,
                content: content.clone(),
                source_agent: Some("ryve sweep".to_string()),
                workshop_id: ws_id.to_string(),
                ttl_seconds: None,
            },
        )
        .await
        {
            Ok(e) => emitted.push(e.id),
            Err(e) => eprintln!("warn: failed to emit ember for {}: {e}", o.spark_id),
        }
    }

    let mut abandoned = Vec::new();
    if abandon {
        for o in &orphans {
            if let Err(e) = assignment_repo::abandon(pool, &o.session_id, &o.spark_id).await {
                eprintln!(
                    "warn: failed to abandon phantom claim {}/{}: {e}",
                    o.spark_id, o.session_id
                );
            } else {
                abandoned.push((o.spark_id.clone(), o.session_id.clone()));
            }
        }
    }

    if json_mode {
        let payload = serde_json::json!({
            "workshop_id": ws_id,
            "orphan_count": orphans.len(),
            "emitted_embers": emitted,
            "abandoned_claims": abandoned,
            "orphans": orphans.iter().map(|o| serde_json::json!({
                "spark_id": o.spark_id,
                "spark_title": o.spark_title,
                "spark_status": o.spark_status,
                "role": o.role,
                "session_id": o.session_id,
                "assigned_at": o.assigned_at,
                "last_heartbeat_at": o.last_heartbeat_at,
                "session_ended_at": o.session_ended_at,
            })).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return;
    }

    if orphans.is_empty() {
        println!("No stalled claims.");
        return;
    }

    println!("{} stalled claim(s):", orphans.len());
    for o in &orphans {
        println!(
            "  {} [{}] role={} session={} status={} ended_at={}",
            o.spark_id,
            o.spark_title,
            o.role,
            o.session_id,
            o.spark_status,
            o.session_ended_at.as_deref().unwrap_or("?"),
        );
    }
    if !emitted.is_empty() {
        println!("emitted {} flare ember(s)", emitted.len());
    }
    if abandon {
        println!("abandoned {} phantom claim(s)", abandoned.len());
    } else if !orphans.is_empty() {
        println!(
            "(pass --abandon to clear phantom assignments; re-running is idempotent via ember dedupe)"
        );
    }
}

// ── Watch ────────────────────────────────────────────
//
// `ryve watch` wires the [`data::sparks::watch_repo`] primitives (sp-ee3f5c74
// S1) into a scriptable CLI surface so Atlas and humans can create, list,
// inspect, cancel, and replace recurring watches from the terminal
// [sp-ee3f5c74]. The CLI never writes SQL itself — every mutation goes
// through the repo.

async fn handle_watch(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() {
        die("watch subcommand required (create, list, show, cancel, replace)");
    }
    match args[0].as_str() {
        "create" => handle_watch_create(pool, &args[1..], json_mode).await,
        "list" | "ls" => handle_watch_list(pool, &args[1..], json_mode).await,
        "show" | "get" => handle_watch_show(pool, &args[1..], json_mode).await,
        "cancel" => handle_watch_cancel(pool, &args[1..], json_mode).await,
        "replace" => handle_watch_replace(pool, &args[1..], json_mode).await,
        other => die(&format!("unknown watch subcommand '{other}'")),
    }
}

/// Parse a cadence token into the typed enum. Accepts either a bare integer
/// (seconds) or the `interval-secs:<N>` / `cron:<expr>` storage form so users
/// can paste back what `watch show` prints.
fn parse_cadence(raw: &str) -> WatchCadence {
    if let Some(c) = WatchCadence::from_storage(raw) {
        return c;
    }
    if let Ok(secs) = raw.parse::<u64>() {
        return WatchCadence::Interval { secs };
    }
    die(&format!(
        "invalid --cadence '{raw}' (expected integer seconds, 'interval-secs:N', or 'cron:<expr>')"
    ));
}

/// Parse a stop-condition spec. Accepts the repo's JSON storage form so a
/// round-trip through `watch show --json` works, plus two ergonomic shorthand
/// forms: `event:<type>` and `status:<spark_id>=<status>`.
fn parse_stop_condition(raw: &str) -> WatchStopCondition {
    if raw.eq_ignore_ascii_case("never") {
        return WatchStopCondition::Never;
    }
    if let Some(c) = WatchStopCondition::from_storage(raw) {
        return c;
    }
    if let Some(event_type) = raw.strip_prefix("event:") {
        return WatchStopCondition::UntilEventType {
            event_type: event_type.to_string(),
        };
    }
    if let Some(rest) = raw.strip_prefix("status:")
        && let Some((spark_id, status)) = rest.split_once('=')
    {
        return WatchStopCondition::UntilSparkStatus {
            spark_id: spark_id.to_string(),
            status: status.to_string(),
        };
    }
    die(&format!(
        "invalid --stop-condition '{raw}' (expected 'never', 'event:<type>', \
         'status:<spark_id>=<status>', or the JSON form from `watch show --json`)"
    ));
}

/// Compute an initial `next_fire_at` for a freshly created watch. Interval
/// cadences are deterministic (`now + secs`); cron expressions are stored
/// unparsed and handed to the scheduler sibling for real evaluation — for now
/// the CLI seeds `next_fire_at = now` so the scheduler tick will pick the
/// watch up immediately and compute the proper instant.
fn initial_next_fire_at(cadence: &WatchCadence) -> String {
    let now = chrono::Utc::now();
    match cadence {
        WatchCadence::Interval { secs } => {
            (now + chrono::Duration::seconds(*secs as i64)).to_rfc3339()
        }
        WatchCadence::Cron { .. } => now.to_rfc3339(),
    }
}

fn print_watch(w: &Watch, json_mode: bool) {
    if json_mode {
        println!("{}", serde_json::to_string_pretty(w).unwrap_or_default());
        return;
    }
    println!("ID:             {}", w.id);
    println!("Target:         {}", w.target_spark_id);
    println!("Cadence:        {}", w.cadence);
    println!(
        "Stop condition: {}",
        w.stop_condition.as_deref().unwrap_or("never")
    );
    println!("Intent:         {}", w.intent_label);
    println!("Status:         {}", w.status);
    println!(
        "Last fired:     {}",
        w.last_fired_at.as_deref().unwrap_or("-")
    );
    println!("Next fire:      {}", w.next_fire_at);
    println!("Created:        {}", w.created_at);
    println!("Updated:        {}", w.updated_at);
    if let Some(by) = &w.created_by {
        println!("Created by:     {by}");
    }
}

fn print_watch_list(rows: &[Watch], json_mode: bool) {
    if json_mode {
        println!("{}", serde_json::to_string_pretty(rows).unwrap_or_default());
        return;
    }
    if rows.is_empty() {
        println!("No watches.");
        return;
    }
    println!(
        "{:<16} {:<22} {:<10} {:<22} {:<24} INTENT",
        "ID", "TARGET", "STATUS", "CADENCE", "NEXT_FIRE_AT"
    );
    println!("{}", "-".repeat(110));
    for w in rows {
        println!(
            "{:<16} {:<22} {:<10} {:<22} {:<24} {}",
            w.id, w.target_spark_id, w.status, w.cadence, w.next_fire_at, w.intent_label,
        );
    }
}

async fn handle_watch_create(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() {
        die("watch create requires <target_spark_id> --cadence <c> --intent <label>");
    }
    let target = args[0].clone();
    let mut cadence: Option<String> = None;
    let mut stop: Option<String> = None;
    let mut intent: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--cadence" => {
                cadence = Some(args.get(i + 1).cloned().unwrap_or_else(|| {
                    die("--cadence requires a value");
                }));
                i += 2;
            }
            "--stop-condition" => {
                stop = Some(args.get(i + 1).cloned().unwrap_or_else(|| {
                    die("--stop-condition requires a value");
                }));
                i += 2;
            }
            "--intent" => {
                intent = Some(args.get(i + 1).cloned().unwrap_or_else(|| {
                    die("--intent requires a value");
                }));
                i += 2;
            }
            other => die(&format!("unknown watch create flag '{other}'")),
        }
    }

    let cadence_raw = cadence.unwrap_or_else(|| die("watch create requires --cadence"));
    let intent_label = intent.unwrap_or_else(|| die("watch create requires --intent"));
    let cadence = parse_cadence(&cadence_raw);
    let stop_condition = stop.map(|s| parse_stop_condition(&s));
    let next_fire_at = initial_next_fire_at(&cadence);

    let new = NewWatch {
        target_spark_id: target,
        cadence,
        stop_condition,
        intent_label,
        next_fire_at,
        created_by: Some("cli".to_string()),
    };

    match watch_repo::create(pool, new).await {
        Ok(w) => {
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&w).unwrap_or_default());
            } else {
                println!("{}", w.id);
            }
        }
        Err(SparksError::DuplicateWatch {
            target_spark_id,
            intent_label,
        }) => {
            // Surface the existing watch id so the caller can inspect or
            // replace it instead of having to re-query the repo.
            let existing_id = watch_repo::list(
                pool,
                WatchFilter {
                    status: Some(WatchStatus::Active),
                    target_spark_id: Some(target_spark_id),
                },
            )
            .await
            .ok()
            .and_then(|rows| {
                rows.into_iter()
                    .find(|w| w.intent_label == intent_label)
                    .map(|w| w.id)
            })
            .unwrap_or_else(|| "unknown".to_string());
            eprintln!("watch already exists: {existing_id}");
            process::exit(1);
        }
        Err(e) => die(&format!("{e}")),
    }
}

async fn handle_watch_list(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    let mut target: Option<String> = None;
    let mut status: Option<WatchStatus> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                target = Some(args.get(i + 1).cloned().unwrap_or_else(|| {
                    die("--target requires a value");
                }));
                i += 2;
            }
            "--status" => {
                let raw = args.get(i + 1).cloned().unwrap_or_else(|| {
                    die("--status requires a value");
                });
                status = Some(WatchStatus::parse(&raw).unwrap_or_else(|| {
                    die(&format!(
                        "invalid --status '{raw}' (expected active, completed, cancelled)"
                    ))
                }));
                i += 2;
            }
            other => die(&format!("unknown watch list flag '{other}'")),
        }
    }

    let filter = WatchFilter {
        status,
        target_spark_id: target,
    };
    match watch_repo::list(pool, filter).await {
        Ok(rows) => print_watch_list(&rows, json_mode),
        Err(e) => die(&format!("{e}")),
    }
}

async fn handle_watch_show(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() {
        die("watch show requires <watch_id>");
    }
    match watch_repo::get(pool, &args[0]).await {
        Ok(w) => print_watch(&w, json_mode),
        Err(e) => die(&format!("{e}")),
    }
}

async fn handle_watch_cancel(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() {
        die("watch cancel requires <watch_id>");
    }
    match watch_repo::cancel(pool, &args[0]).await {
        Ok(w) => {
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&w).unwrap_or_default());
            } else {
                println!("cancelled {}", w.id);
            }
        }
        Err(e) => die(&format!("{e}")),
    }
}

async fn handle_watch_replace(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() {
        die("watch replace requires <watch_id> --cadence <c>");
    }
    let existing_id = args[0].clone();
    let mut cadence: Option<String> = None;
    let mut stop_raw: Option<String> = None;
    let mut stop_flag_seen = false;
    let mut intent: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--cadence" => {
                cadence = Some(args.get(i + 1).cloned().unwrap_or_else(|| {
                    die("--cadence requires a value");
                }));
                i += 2;
            }
            "--stop-condition" => {
                stop_flag_seen = true;
                stop_raw = Some(args.get(i + 1).cloned().unwrap_or_else(|| {
                    die("--stop-condition requires a value");
                }));
                i += 2;
            }
            "--intent" => {
                intent = Some(args.get(i + 1).cloned().unwrap_or_else(|| {
                    die("--intent requires a value");
                }));
                i += 2;
            }
            other => die(&format!("unknown watch replace flag '{other}'")),
        }
    }

    let existing = match watch_repo::get(pool, &existing_id).await {
        Ok(w) => w,
        Err(e) => die(&format!("{e}")),
    };

    // Missing flags inherit from the existing watch so callers can re-tune a
    // single axis without re-specifying the rest.
    let cadence = match cadence {
        Some(c) => parse_cadence(&c),
        None => existing.parsed_cadence().unwrap_or_else(|| {
            die(&format!(
                "existing watch {} has malformed cadence '{}' — specify --cadence explicitly",
                existing.id, existing.cadence,
            ))
        }),
    };
    let stop_condition = if stop_flag_seen {
        stop_raw.map(|s| parse_stop_condition(&s))
    } else {
        existing.parsed_stop_condition().and_then(|sc| match sc {
            WatchStopCondition::Never => None,
            other => Some(other),
        })
    };
    let intent_label = intent.unwrap_or_else(|| existing.intent_label.clone());
    let next_fire_at = initial_next_fire_at(&cadence);

    let new = NewWatch {
        target_spark_id: existing.target_spark_id.clone(),
        cadence,
        stop_condition,
        intent_label,
        next_fire_at,
        created_by: Some("cli".to_string()),
    };

    match watch_repo::replace(pool, &existing_id, new).await {
        Ok(w) => {
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&w).unwrap_or_default());
            } else {
                println!("{}", w.id);
            }
        }
        Err(e) => die(&format!("{e}")),
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
        die("assign subcommand required (claim, release, list, override)");
    }
    match args[0].as_str() {
        "claim" => {
            if args.len() < 3 {
                die("assign claim requires <session_id> <spark_id>");
            }
            let sid = &args[1];
            let new = NewHandAssignment {
                session_id: sid.clone(),
                spark_id: args[2].clone(),
                role: AssignmentRole::Owner,
                actor_id: None,
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
        "override" => handle_assignment_override(pool, &args[1..], json_mode).await,
        other => die(&format!("unknown assign subcommand '{other}'")),
    }
}

/// Handle `ryve assign override <session_id> <spark_id> --to in_progress --reason <text>`.
///
/// Head/Director only. Finds the most-recent assignment on `<spark_id>`,
/// confirms the caller session is a Head or Director (via
/// `agent_sessions.session_label`), and drives the
/// `Stuck → InProgress` override through
/// [`data::sparks::assign_repo::override_stuck_to_in_progress`]. The
/// override reason is audit-logged immediately after the phase
/// transition (see the underlying function's docs for the current
/// non-atomic guarantee and the tech debt to make it a single tx).
async fn handle_assignment_override(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.len() < 2 {
        die("assign override requires <session_id> <spark_id> \
             --to in_progress --reason <text>");
    }
    let session_id = args[0].clone();
    let spark_id = args[1].clone();

    let mut target: Option<String> = None;
    let mut reason: Option<String> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--to" => {
                if i + 1 >= args.len() {
                    die("--to requires a value (supported: in_progress)");
                }
                target = Some(args[i + 1].clone());
                i += 2;
            }
            "--reason" => {
                if i + 1 >= args.len() {
                    die("--reason requires a value");
                }
                reason = Some(args[i + 1].clone());
                i += 2;
            }
            other => die(&format!("unknown flag '{other}' for assign override")),
        }
    }

    let target = target.unwrap_or_else(|| die("assign override requires --to in_progress"));
    if target != "in_progress" {
        die(&format!(
            "assign override only supports --to in_progress (got '{target}')"
        ));
    }
    let reason = reason.unwrap_or_else(|| die("assign override requires --reason <text>"));
    if reason.trim().is_empty() {
        die("--reason must not be empty; the override is audit-logged");
    }

    // Derive the caller's TransitionActorRole from the session label.
    // Only "head" and "director" are accepted — every other label (or
    // missing label) is rejected so Hands cannot run the recovery path.
    let actor_role = match agent_session_repo::get(pool, &session_id).await {
        Ok(Some(s)) => match s.session_label.as_deref() {
            Some("head") => TransitionActorRole::Head,
            Some("director") => TransitionActorRole::Director,
            other => die(&format!(
                "assign override rejected: session {session_id} is labelled '{}' \
                 but only head/director sessions may override a stuck assignment",
                other.unwrap_or("<none>")
            )),
        },
        Ok(None) => die(&format!(
            "assign override rejected: session {session_id} not found in agent_sessions"
        )),
        Err(e) => die(&format!("failed to look up session {session_id}: {e}")),
    };

    let assignment =
        match data::sparks::assign_repo::latest_assignment_for_spark(pool, &spark_id).await {
            Ok(a) => a,
            Err(e) => die(&format!(
                "assign override failed to resolve assignment for {spark_id}: {e}"
            )),
        };

    match data::sparks::assign_repo::override_stuck_to_in_progress(
        pool,
        &assignment.assignment_id,
        &session_id,
        actor_role,
        &reason,
    )
    .await
    {
        Ok(updated) => {
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&updated).unwrap_or_default()
                );
            } else {
                println!(
                    "override: {} restored to in_progress by {} ({})",
                    updated.assignment_id,
                    session_id,
                    actor_role.as_str()
                );
                println!("reason: {reason}");
            }
        }
        Err(e) => die(&format!("assign override failed: {e}")),
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
            // Archetype policy gate (spark ryve-e6713ee7): a Release
            // Manager may not spawn any Hand. Enforced here, before any
            // DB write or worktree creation, so the refusal leaves no
            // trace in the workgraph.
            enforce_or_die(
                current_caller_archetype(pool).await,
                &PolicyAction::SpawnHand,
            );
            if args.len() >= 2 && matches!(args[1].as_str(), "help" | "--help" | "-h") {
                print_hand_spawn_usage();
                return;
            }
            if args.len() < 2 {
                die(
                    "hand spawn requires <spark_id> [--agent <name>] [--role owner|head|investigator|architect|reviewer|release_manager|bug_hunter|performance_engineer|merger] [--crew <id>]. Try `ryve hand spawn --help`.",
                );
            }
            let spark_id = args[1].clone();
            let mut agent_name: Option<String> = None;
            let mut role = HandKind::Owner;
            let mut crew_id: Option<String> = None;
            let mut actor_override: Option<String> = None;
            // Reviewer-only inputs. `candidates` is a repeatable
            // `--candidate actor:vendor` flag; `author_actor` /
            // `author_vendor` name the Hand whose work is under review;
            // `seed` is the deterministic tiebreaker (default 0).
            let mut author_actor: Option<String> = None;
            let mut author_vendor: Option<String> = None;
            let mut reviewer_candidates: Vec<hand_spawn::ReviewerCandidate> = Vec::new();
            let mut reviewer_seed: u64 = 0;
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
                                "investigator" => HandKind::Investigator,
                                "architect" => HandKind::Architect,
                                "reviewer" => HandKind::Reviewer,
                                "release_manager" | "release-manager" => HandKind::ReleaseManager,
                                "bug_hunter" | "bug-hunter" => HandKind::BugHunter,
                                "performance_engineer"
                                | "performance-engineer"
                                | "perf_engineer"
                                | "perf-engineer" => HandKind::PerformanceEngineer,
                                "merger" => HandKind::Merger,
                                other => die(&format!(
                                    "invalid role '{other}' (owner|head|investigator|architect|reviewer|release_manager|bug_hunter|performance_engineer|merger)"
                                )),
                            };
                        }
                    }
                    "--crew" => {
                        i += 1;
                        if i < args.len() {
                            crew_id = Some(args[i].clone());
                        }
                    }
                    "--actor" => {
                        i += 1;
                        if i < args.len() {
                            actor_override = Some(args[i].clone());
                        }
                    }
                    "--author-actor" => {
                        i += 1;
                        if i < args.len() {
                            author_actor = Some(args[i].clone());
                        }
                    }
                    "--author-vendor" => {
                        i += 1;
                        if i < args.len() {
                            author_vendor = Some(args[i].clone());
                        }
                    }
                    "--candidate" => {
                        i += 1;
                        if i < args.len() {
                            let raw = &args[i];
                            let (actor, vendor) = raw.split_once(':').unwrap_or_else(|| {
                                die(&format!(
                                    "--candidate '{raw}' must be <actor>:<vendor> \
                                     (e.g. alice:claude)"
                                ))
                            });
                            reviewer_candidates.push(hand_spawn::ReviewerCandidate {
                                actor_id: actor.trim().to_string(),
                                vendor: vendor.trim().to_string(),
                            });
                        }
                    }
                    "--seed" => {
                        i += 1;
                        if i < args.len() {
                            reviewer_seed = args[i].parse().unwrap_or_else(|_| {
                                die(&format!("--seed '{}' must be a u64", args[i]))
                            });
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

            // Reviewer spawns dispatch through the policy-aware
            // `spawn_reviewer` path (deterministic cross-vendor-preferring
            // selection, `reviewer_policy_relaxed` on same-vendor fallback,
            // `awaiting_reviewer_availability` + flare ember when no
            // reviewer is eligible). Standard Hand kinds use the generic
            // `spawn_hand` path.
            if matches!(role, HandKind::Reviewer) {
                let Some(aa) = author_actor.as_deref() else {
                    die("--role reviewer requires --author-actor <id>");
                };
                let Some(av) = author_vendor.as_deref() else {
                    die("--role reviewer requires --author-vendor <vendor>");
                };
                match hand_spawn::spawn_reviewer(
                    workshop_root,
                    pool,
                    &agent,
                    &spark_id,
                    hand_spawn::ReviewerRequest {
                        author_actor: aa,
                        author_vendor: av,
                        candidates: &reviewer_candidates,
                        seed: reviewer_seed,
                    },
                    hand_spawn::SpawnContext {
                        crew_id: crew_id.as_deref(),
                        parent_session_id: parent_session_id.as_deref(),
                        actor_id: actor_override.as_deref(),
                    },
                )
                .await
                {
                    Ok(hand_spawn::ReviewerSpawnOutcome::Spawned { hand, selection }) => {
                        if json_mode {
                            let payload = serde_json::json!({
                                "outcome": "spawned",
                                "selection": selection.as_str(),
                                "session_id": hand.session_id,
                                "spark_id": hand.spark_id,
                                "worktree": hand.worktree_path,
                                "log": hand.log_path,
                                "pid": hand.child_pid,
                            });
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&payload).unwrap_or_default()
                            );
                        } else {
                            println!(
                                "spawned reviewer {} on spark {} (selection {}, pid {:?})",
                                hand.session_id,
                                hand.spark_id,
                                selection.as_str(),
                                hand.child_pid
                            );
                            println!("  worktree: {}", hand.worktree_path.display());
                            println!("  log:      {}", hand.log_path.display());
                        }
                    }
                    Ok(hand_spawn::ReviewerSpawnOutcome::AwaitingAvailability { ember_id }) => {
                        if json_mode {
                            let payload = serde_json::json!({
                                "outcome": "awaiting_reviewer_availability",
                                "spark_id": spark_id,
                                "ember_id": ember_id,
                            });
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&payload).unwrap_or_default()
                            );
                        } else {
                            println!(
                                "spark {spark_id}: awaiting_reviewer_availability \
                                 (ember {ember_id}) — no eligible reviewer in pool"
                            );
                        }
                    }
                    Err(e) => die(&format!("{e}")),
                }
            } else {
                match hand_spawn::spawn_hand(
                    workshop_root,
                    pool,
                    &agent,
                    &spark_id,
                    role,
                    hand_spawn::SpawnContext {
                        crew_id: crew_id.as_deref(),
                        parent_session_id: parent_session_id.as_deref(),
                        actor_id: actor_override.as_deref(),
                    },
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
        // Durable heartbeat loop for a spawned Hand (spark ryve-85034c27).
        // This subcommand is the sidecar body: each active-assignment tick
        // emits a `HeartbeatReceived` event into `event_outbox` and advances
        // `assignments.last_heartbeat_at`. The loop exits cleanly the first
        // time the assignment stops being active (Hand closed its spark,
        // Head override, sweep-reaper, etc.).
        "heartbeat-loop" => {
            if args.len() < 3 {
                die("hand heartbeat-loop requires <session_id> <spark_id> \
                     [--interval-secs N] [--max-ticks N]");
            }
            let session_id = args[1].clone();
            let spark_id = args[2].clone();
            let mut interval_secs: Option<u64> = None;
            let mut max_ticks: Option<u64> = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--interval-secs" => {
                        i += 1;
                        if i >= args.len() {
                            die("hand heartbeat-loop --interval-secs requires a value");
                        }
                        interval_secs = Some(args[i].parse().unwrap_or_else(|_| {
                            die(&format!("--interval-secs '{}' must be u64", args[i]))
                        }));
                    }
                    "--max-ticks" => {
                        i += 1;
                        if i >= args.len() {
                            die("hand heartbeat-loop --max-ticks requires a value");
                        }
                        max_ticks = Some(args[i].parse().unwrap_or_else(|_| {
                            die(&format!("--max-ticks '{}' must be u64", args[i]))
                        }));
                    }
                    other => die(&format!("unknown hand heartbeat-loop flag '{other}'")),
                }
                i += 1;
            }

            // Resolve the per-workshop interval override; fall back to the
            // default so CLI callers don't have to know it. CLI `--interval-secs`
            // beats config so tests can pick a short cadence.
            let ryve_dir = data::ryve_dir::RyveDir::new(workshop_root);
            let config = data::ryve_dir::load_config(&ryve_dir).await;
            let interval = std::time::Duration::from_secs(
                interval_secs.unwrap_or(config.heartbeat_interval_secs),
            );

            let mut ticks: u64 = 0;
            loop {
                match data::sparks::heartbeat::emit_heartbeat(pool, &session_id, &spark_id).await {
                    Ok(data::sparks::heartbeat::HeartbeatOutcome::Emitted) => {
                        ticks += 1;
                        if let Some(cap) = max_ticks
                            && ticks >= cap
                        {
                            break;
                        }
                    }
                    Ok(data::sparks::heartbeat::HeartbeatOutcome::AssignmentInactive) => {
                        break;
                    }
                    Err(e) => {
                        // Log and keep trying — a transient DB lock shouldn't
                        // kill the loop. If the pool is permanently gone the
                        // next sleep + retry will exit via connection error
                        // cascade; we don't want a single hiccup to silence
                        // the Hand's liveness signal.
                        eprintln!(
                            "hand heartbeat-loop: emit failed ({e}); retrying after interval"
                        );
                    }
                }
                tokio::time::sleep(interval).await;
            }
            if json_mode {
                let payload = serde_json::json!({
                    "session_id": session_id,
                    "spark_id": spark_id,
                    "ticks": ticks,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            } else {
                println!(
                    "hand heartbeat-loop exited: session={session_id} spark={spark_id} ticks={ticks}"
                );
            }
        }
        // Run a subprocess under the StreamHeartbeat wrapper so long-silent
        // Hand-side subcommands (notably the full-workspace `cargo test`)
        // do not trip Claude's ~5-minute stream-idle timeout and kill the
        // Hand session. Opt-in per invocation: a Hand calls this
        // explicitly for commands it knows will be silent for minutes at
        // a time, and stays on the plain shell for short commands so
        // their output is not polluted with heartbeat lines. Parent epic
        // ryve-32e95c28 / [sp-b7f7f1fa].
        "exec-heartbeat" => {
            let mut interval_secs: Option<u64> = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--interval-secs" => {
                        i += 1;
                        if i >= args.len() {
                            die("hand exec-heartbeat --interval-secs requires a value");
                        }
                        interval_secs = Some(args[i].parse().unwrap_or_else(|_| {
                            die(&format!(
                                "--interval-secs '{}' must be a non-negative integer",
                                args[i]
                            ))
                        }));
                        i += 1;
                    }
                    "--" => {
                        i += 1;
                        break;
                    }
                    "--help" | "-h" => {
                        println!(
                            "ryve hand exec-heartbeat — run a subprocess under the \
                             stream-heartbeat wrapper\n\n\
                             USAGE:\n  \
                             ryve hand exec-heartbeat [--interval-secs N] -- <cmd> [args...]\n\n\
                             Forwards the child's stdout and stderr to this process's \
                             stdout byte-for-byte while injecting a heartbeat line \
                             (default every 30 s) whenever the child is silent. Exits \
                             with the child's exit code. Use it when a Hand runs \
                             a command that may stay silent past Claude's \
                             ~5-minute stream-idle threshold (e.g. the full \
                             workspace `cargo test`)."
                        );
                        return;
                    }
                    other if other.starts_with("--") => {
                        die(&format!("unknown hand exec-heartbeat flag '{other}'"));
                    }
                    _ => break,
                }
            }
            if i >= args.len() {
                die("hand exec-heartbeat requires a command: \
                     `ryve hand exec-heartbeat [--interval-secs N] -- <cmd> [args...]`");
            }
            let cmd = args[i].clone();
            let cmd_args: Vec<&str> = args[(i + 1)..].iter().map(String::as_str).collect();
            let interval = interval_secs.map(std::time::Duration::from_secs);

            let mut stdout = tokio::io::stdout();
            match hand_spawn::run_with_stream_heartbeat(
                &cmd,
                &cmd_args,
                None,
                &[],
                interval,
                &mut stdout,
            )
            .await
            {
                Ok(outcome) => {
                    // Propagate the child's exit code so the caller
                    // (typically a Hand's shell) sees a failed
                    // `cargo test` as a non-zero exit. Signal-killed
                    // children (no exit code) surface as 1.
                    std::process::exit(outcome.status.code().unwrap_or(1));
                }
                Err(e) => die(&format!("hand exec-heartbeat: {e}")),
            }
        }
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
  ryve hand spawn <spark_id> [--agent <name>] [--role owner|head|investigator|architect|reviewer|release_manager|bug_hunter|performance_engineer|merger] [--crew <id>]
  ryve hand list
  ryve hand --help
  ryve hand spawn --help

SUBCOMMANDS:
  spawn           Launch a detached Hand subprocess on a spark.
  list            Show active hand assignments (ownership + role + heartbeat).
  exec-heartbeat  Run a subprocess under the stream-heartbeat wrapper so long
                  silent commands (e.g. full-workspace `cargo test`) do not
                  trip Claude's ~5-minute stream-idle timeout.
                  See `ryve hand exec-heartbeat --help`.

ROLES:
  owner             (default) Standard worker. Claims the spark, works in its
                    own worktree, closes the spark when DONE.md passes.
  head              Crew orchestrator. Takes an epic, decomposes it into child
                    sparks, spawns sub-Hands, and supervises them. Prefer the
                    dedicated `ryve head spawn` wrapper — it reads more naturally.
  investigator      Read-only auditor. Sweeps code and posts findings as
                    comments on the parent spark; may not edit files or run
                    destructive git. Typically spawned by a Research Head.
  architect         Read-only design reviewer. Produces recommendations,
                    tradeoffs, and risks as structured comments on the parent
                    spark; never edits code. Typically spawned by a Review Head.
  reviewer          Read-only code reviewer. Approves or rejects a spark's
                    AwaitingReview phase against its acceptance criteria.
                    Spawned via the Reviewer spawn path with deterministic,
                    cross-vendor-preferring selection (see
                    `hand_spawn::spawn_reviewer`).
  release_manager   Atlas-only Release steward. Runs `ryve release *`, commits
                    to `release/*`, comments on release member sparks only.
                    Cannot spawn Hands/Heads, cannot send embers. Singleton per
                    release (see docs/HAND_ARCHETYPES.md).
  bug_hunter        Triager+Surgeon hybrid specialised on small defects.
                    Reproduces the bug with a failing test, lands the smallest
                    possible diff that flips it green. Write-capable; scope is
                    policed by the prompt, not a CLI gate.
  performance_engineer
                    Refactorer+Cartographer hybrid specialised on measurable
                    improvements. Baselines, profiles, proposes, and verifies,
                    recording before/after numbers as spark comments. Write-
                    capable; scope is policed by the prompt, not a CLI gate.
  merger            Crew integrator. Collects sibling worktrees, merges them
                    into a single `crew/<id>` branch, and opens one PR.
                    Requires --crew.

See also:
  ryve head --help                    Head-specific documentation.
  docs/AGENT_HIERARCHY.md              Atlas → Head → Hand overview.
  docs/HAND_CAPABILITIES.md            Hand capability classes.
  docs/HAND_ARCHETYPES.md              Concrete Hand archetypes (Architect, …).
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
  --role <role>            owner | head | investigator | architect | reviewer | release_manager | bug_hunter | performance_engineer | merger.
                           Default: owner.
                             owner            — standard worker Hand.
                             head             — crew orchestrator (prefer `ryve head spawn`).
                             investigator     — read-only auditor; posts findings via comments.
                             architect        — read-only design reviewer; posts recommendations,
                                                tradeoffs, and risks as comments (see docs/HAND_ARCHETYPES.md).
                             reviewer         — read-only code reviewer; approves/rejects
                                                AwaitingReview (see hand_spawn::spawn_reviewer).
                             release_manager  — Atlas-only Release steward; see docs/HAND_ARCHETYPES.md.
                             bug_hunter       — Triager+Surgeon hybrid; failing test → smallest diff.
                             performance_engineer
                                              — Refactorer+Cartographer hybrid; baseline → profile → propose → verify.
                             merger           — crew integrator (requires --crew).
  --crew <crew_id>         Attach the new Hand to an existing crew as a
                           member. Required when --role merger.
  --author-actor <id>      (--role reviewer) Actor of the Hand whose work is
                           under review. The reviewer is selected to be
                           different from this actor.
  --author-vendor <vendor> (--role reviewer) Vendor of the author (e.g.
                           claude, codex). The selection prefers a
                           cross-vendor reviewer and falls back to
                           same-vendor with a reviewer_policy_relaxed
                           event recorded on the spark.
  --candidate <actor:vendor>
                           (--role reviewer, repeatable) Reviewer pool
                           entry. Pass one per candidate; the author is
                           excluded automatically.
  --seed <u64>             (--role reviewer) Deterministic tiebreaker for
                           selection. Same inputs + same seed always
                           produce the same reviewer. Default: 0.

EFFECTS:
  1. Creates a git worktree at `.ryve/worktrees/<short>/` on branch
     `hand/<short>`.
  2. Persists an `agent_sessions` row (session_label = hand / head /
     investigator / architect / release_manager / bug_hunter / performance_engineer / merger)
     and a `hand_assignments` row claiming the spark.
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
  ryve head spawn <epic_id> --archetype <build|research|review> [--agent <name>] [--crew <id>]
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
  --archetype <name>  Head archetype: build, research, or review.
                      Defaults to build. See `ryve head archetype list`.
  --agent <name>      Coding agent to run as the Head (claude, codex, aider,
                      opencode, …). Defaults to the first detected agent.
  --crew <id>         Attach the Head to an existing crew. Optional: most
                      workflows create the crew from inside the Head itself
                      via `ryve crew create --head-session $RYVE_SESSION_ID`.

EFFECTS:
  1. Creates a git worktree for the Head at `.ryve/worktrees/<short>/`.
  2. Persists an `agent_sessions` row with `session_label = \"head\"`.
  3. Creates or reuses a Crew (via `--crew`), registers the Head as a
     crew member with role \"head\", and sets `head_session_id` on the
     Crew row. No `hand_assignments` row is created — the Head's
     relationship to the workgraph is carried by the Crew, not a spark
     claim.
  4. Writes the Head system prompt (composed via `compose_head_prompt`,
     injecting the epic id + title + archetype) to `.ryve/prompts/head-<id>.md`.
  5. Launches the agent in full-auto, detached, with stdout/stderr going
     to `.ryve/logs/head-<id>.log`.

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

/// Consolidated `ryve head` dispatcher. Combines spawn (ryve-e4cadc03),
/// list, archetype (ryve-982bddb8), orchestrate (ryve-85945fa3), and render.
async fn handle_head(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    ws_id: &str,
    args: &[String],
    json_mode: bool,
) {
    if args.is_empty() {
        print_head_usage();
        process::exit(1);
    }
    if matches!(args[0].as_str(), "help" | "--help" | "-h") {
        print_head_usage();
        return;
    }
    match args[0].as_str() {
        "spawn" => {
            // Archetype policy gate (spark ryve-e6713ee7): Release
            // Manager may not spawn a Head either.
            enforce_or_die(
                current_caller_archetype(pool).await,
                &PolicyAction::SpawnHead,
            );
            if args.len() >= 2 && matches!(args[1].as_str(), "help" | "--help" | "-h") {
                print_head_spawn_usage();
                return;
            }
            if args.len() < 2 {
                die(
                    "head spawn requires <epic_id> --archetype <build|research|review> [--agent <name>] [--crew <id>]. Try `ryve head spawn --help`.",
                );
            }
            let epic_id = args[1].clone();
            let mut archetype_name: Option<String> = None;
            let mut agent_name: Option<String> = None;
            let mut crew_id: Option<String> = None;
            let mut actor_override: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--archetype" => {
                        i += 1;
                        if i < args.len() {
                            archetype_name = Some(args[i].clone());
                        }
                    }
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
                    "--actor" => {
                        i += 1;
                        if i < args.len() {
                            actor_override = Some(args[i].clone());
                        }
                    }
                    other => die(&format!(
                        "unknown head spawn flag '{other}'. Try `ryve head spawn --help`."
                    )),
                }
                i += 1;
            }

            let archetype = match archetype_name.as_deref() {
                Some(name) => HeadArchetype::from_str(name).unwrap_or_else(|| {
                    die(&format!(
                        "unknown archetype '{name}': expected build, research, or review"
                    ))
                }),
                None => die(
                    "head spawn requires --archetype <build|research|review> (see docs/HEAD_ARCHETYPES.md)",
                ),
            };

            let agent = resolve_agent(agent_name.as_deref());
            let parent_session_id = std::env::var("RYVE_HAND_SESSION_ID").ok();

            match hand_spawn::spawn_head(
                workshop_root,
                pool,
                &agent,
                &epic_id,
                archetype,
                hand_spawn::SpawnContext {
                    crew_id: crew_id.as_deref(),
                    parent_session_id: parent_session_id.as_deref(),
                    actor_id: actor_override.as_deref(),
                },
            )
            .await
            {
                Ok(spawned) => {
                    if json_mode {
                        let payload = serde_json::json!({
                            "session_id": spawned.session_id,
                            "epic_id": spawned.epic_id,
                            "crew_id": spawned.crew_id,
                            "archetype": spawned.archetype.as_str(),
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
                            "spawned {} head {} on epic {} (crew {}, pid {:?})",
                            spawned.archetype.as_str(),
                            spawned.session_id,
                            spawned.epic_id,
                            spawned.crew_id,
                            spawned.child_pid
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
        "list" | "ls" => match agent_session_repo::list_for_workshop(pool, ws_id).await {
            Ok(sessions) => {
                let heads: Vec<_> = sessions
                    .into_iter()
                    .filter(|s| s.session_label.as_deref() == Some("head"))
                    .collect();
                if json_mode {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&heads).unwrap_or_default()
                    );
                } else if heads.is_empty() {
                    println!("No Head sessions.");
                } else {
                    println!("{:<36} {:<20} {:<10} STARTED", "SESSION", "AGENT", "STATUS");
                    let sep = "-".repeat(90);
                    println!("{sep}");
                    for s in &heads {
                        println!(
                            "{:<36} {:<20} {:<10} {}",
                            s.id, s.agent_name, s.status, s.started_at
                        );
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        },
        "archetype" | "archetypes" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("list");
            match sub {
                "list" | "ls" => {
                    let registry = crate::head_archetype::Registry::builtins();
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(registry.all()).unwrap_or_default()
                        );
                    } else if registry.all().is_empty() {
                        println!("No Head archetypes registered.");
                    } else {
                        println!(
                            "{:<12} {:<18} {:<22} DESCRIPTION",
                            "NAME", "DEFAULT AGENT", "WRITE DISCIPLINE"
                        );
                        let sep = "-".repeat(96);
                        println!("{sep}");
                        for a in registry.all() {
                            let discipline = format!("{:?}", a.write_discipline);
                            println!(
                                "{:<12} {:<18} {:<22} {}",
                                a.name, a.default_agent, discipline, a.description
                            );
                        }
                    }
                }
                other => die(&format!("unknown head archetype subcommand '{other}'")),
            }
        }
        "orchestrate" => {
            handle_head_orchestrate(pool, workshop_root, ws_id, &args[1..], json_mode).await;
        }
        "render" => {
            let archetype_id = args
                .get(1)
                .map(|s| s.as_str())
                .unwrap_or_else(|| die("usage: ryve head render <archetype> --epic <epic_id>"));
            let mut epic_id: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--epic" => {
                        epic_id = args.get(i + 1).cloned();
                        i += 2;
                    }
                    other => {
                        die(&format!("unknown flag for `head render`: {other}"));
                    }
                }
            }
            let epic_id = epic_id
                .unwrap_or_else(|| die("missing --epic <epic_id> (required for head render)"));
            let archetype = crate::head_archetypes::find(archetype_id).unwrap_or_else(|| {
                die(&format!(
                    "unknown archetype '{archetype_id}'. Known archetypes: 'build', 'research', 'review' (see `ryve head archetype list`)"
                ))
            });
            eprintln!(
                "rendering {} ({}) — template: {}",
                archetype.display_name, archetype.description, archetype.template_path
            );
            let rendered = archetype.render(&epic_id);
            print!("{rendered}");
            if !rendered.ends_with('\n') {
                println!();
            }
        }
        other => die(&format!(
            "unknown head subcommand '{other}'. Try `ryve head --help`."
        )),
    }
}

async fn handle_head_orchestrate(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    ws_id: &str,
    args: &[String],
    json_mode: bool,
) {
    use crate::head::orchestrator::{self, OrchestrationConfig};

    if args.is_empty() {
        die(
            "head orchestrate requires <parent_spark_id> [--children <csv>] \
             [--merge-spark <id>] [--crew-name <name>] [--purpose <text>] \
             [--agent <name>] [--stall-seconds N] [--poll-seconds M] [--max-cycles N]",
        );
    }
    let parent_spark_id = args[0].clone();

    let mut children: Vec<String> = Vec::new();
    let mut merge_spark: Option<String> = None;
    let mut crew_name: Option<String> = None;
    let mut purpose: Option<String> = None;
    let mut agent_name: Option<String> = None;
    let mut cfg = OrchestrationConfig::default();
    let mut max_cycles: u32 = 120; // default ~2h at 60s poll
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--children" => {
                i += 1;
                if i < args.len() {
                    children = args[i]
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
            "--merge-spark" => {
                i += 1;
                if i < args.len() {
                    merge_spark = Some(args[i].clone());
                }
            }
            "--crew-name" => {
                i += 1;
                if i < args.len() {
                    crew_name = Some(args[i].clone());
                }
            }
            "--purpose" => {
                i += 1;
                if i < args.len() {
                    purpose = Some(args[i].clone());
                }
            }
            "--agent" => {
                i += 1;
                if i < args.len() {
                    agent_name = Some(args[i].clone());
                }
            }
            "--stall-seconds" => {
                i += 1;
                if i < args.len()
                    && let Ok(n) = args[i].parse::<u64>()
                {
                    cfg.stall_after = std::time::Duration::from_secs(n);
                }
            }
            "--poll-seconds" => {
                i += 1;
                if i < args.len()
                    && let Ok(n) = args[i].parse::<u64>()
                {
                    cfg.poll_interval = std::time::Duration::from_secs(n);
                }
            }
            "--max-cycles" => {
                i += 1;
                if i < args.len()
                    && let Ok(n) = args[i].parse::<u32>()
                {
                    max_cycles = n;
                }
            }
            other => die(&format!("unknown head orchestrate flag '{other}'")),
        }
        i += 1;
    }

    if children.is_empty() {
        die("head orchestrate requires --children <spark_id[,spark_id...]>");
    }

    let agent = resolve_agent(agent_name.as_deref());
    let head_session_id = std::env::var("RYVE_HAND_SESSION_ID").ok();
    let name = crew_name.unwrap_or_else(|| format!("crew-{parent_spark_id}"));

    // 1. spawn_crew
    let mut crew = match orchestrator::spawn_crew(
        workshop_root,
        pool,
        &agent,
        ws_id,
        &name,
        purpose.as_deref(),
        Some(&parent_spark_id),
        &children,
        head_session_id.as_deref(),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => die(&format!("spawn_crew failed: {e}")),
    };

    if !json_mode {
        println!(
            "spawned crew {} with {} member(s)",
            crew.crew_id,
            crew.spark_ids.len()
        );
    }

    // 2. poll / reassign loop
    let mut respawn_counts: std::collections::HashMap<String, u32> = Default::default();
    let mut cycles = 0u32;
    let final_report = loop {
        if let Err(e) = orchestrator::drop_closed_siblings(pool, &mut crew).await {
            eprintln!("warn: drop_closed_siblings failed: {e}");
        }
        let report = match orchestrator::poll_crew(pool, &crew, &cfg).await {
            Ok(r) => r,
            Err(e) => die(&format!("poll_crew failed: {e}")),
        };

        if !json_mode {
            println!(
                "[cycle {cycles}] total={} done={} stalled={} unassigned={} stuck={}",
                report.total(),
                report.done(),
                report.stalled_spark_ids().len(),
                report.unassigned_spark_ids().len(),
                report.stuck_members().len(),
            );
        }

        // Stuck assignments do NOT feed into the reassign path — only
        // a Head/Director override (`ryve assign override`) can recover
        // them. Surface them as an ember + a comment on the parent Epic
        // so the override actor sees the block.
        if !report.stuck_members().is_empty() {
            match crate::head::orchestrator::notify_stuck(pool, ws_id, &crew, &report).await {
                Ok(n) if !json_mode => {
                    println!("  notified {n} stuck member(s) on the parent epic");
                }
                Ok(_) => {}
                Err(e) => eprintln!("warn: notify_stuck failed: {e}"),
            }
        }

        if report.all_done() {
            break report;
        }

        if !report.stalled_spark_ids().is_empty() || !report.unassigned_spark_ids().is_empty() {
            match orchestrator::reassign_stalled(
                workshop_root,
                pool,
                &agent,
                &mut crew,
                &report,
                &cfg,
                &mut respawn_counts,
                head_session_id.as_deref(),
            )
            .await
            {
                Ok(r) => {
                    if !json_mode && !r.respawned.is_empty() {
                        println!("  reassigned {} hand(s)", r.respawned.len());
                    }
                    if !r.capped.is_empty() {
                        eprintln!("  respawn cap reached for: {:?}", r.capped);
                    }
                }
                Err(e) => die(&format!("reassign_stalled failed: {e}")),
            }
        }

        cycles += 1;
        if cycles >= max_cycles {
            eprintln!("max-cycles ({max_cycles}) reached; exiting loop");
            break report;
        }
        tokio::time::sleep(cfg.poll_interval).await;
    };

    // 3. finalize_with_merger (optional — Head may not have a merge spark yet)
    let merger_info = if final_report.all_done() {
        match merge_spark.as_deref() {
            Some(msid) => match orchestrator::finalize_with_merger(
                workshop_root,
                pool,
                &agent,
                &crew,
                msid,
                head_session_id.as_deref(),
            )
            .await
            {
                Ok(spawned) => {
                    if !json_mode {
                        println!(
                            "spawned merger session {} on merge spark {}",
                            spawned.session_id, spawned.spark_id
                        );
                    }
                    Some(spawned)
                }
                Err(e) => die(&format!("finalize_with_merger failed: {e}")),
            },
            None => {
                eprintln!("all sparks done but --merge-spark not provided; skipping merger spawn");
                None
            }
        }
    } else {
        None
    };

    if json_mode {
        let payload = serde_json::json!({
            "crew_id": crew.crew_id,
            "parent_spark_id": crew.parent_spark_id,
            "spark_ids": crew.spark_ids,
            "owners": crew.owners,
            "cycles": cycles,
            "all_done": final_report.all_done(),
            "total": final_report.total(),
            "done": final_report.done(),
            "merger_session_id": merger_info.as_ref().map(|m| m.session_id.clone()),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
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

/// `ryve tmux` — tmux session introspection and reconciliation.
///
/// Primarily consumed by the integration tests (sp-0285181c) so they can
/// exercise the real production code paths (`tmux::reconcile_sessions` and
/// `TmuxClient::attach_command`) from a separate process. Also a useful
/// admin tool when Ryve's UI is not running.
async fn handle_tmux(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    ws_id: &str,
    args: &[String],
    json_mode: bool,
) {
    if args.is_empty() {
        die("tmux subcommand required (reconcile, attach-cmd, has-session)");
    }
    match args[0].as_str() {
        "reconcile" => {
            let result = crate::tmux::reconcile_sessions(workshop_root, pool, ws_id).await;
            if json_mode {
                let payload = serde_json::json!({
                    "confirmed_live": result.confirmed_live,
                    "marked_stopped": result.marked_stopped,
                    "orphaned_tmux": result.orphaned_tmux,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            } else {
                println!("confirmed_live: {} session(s)", result.confirmed_live.len());
                for id in &result.confirmed_live {
                    println!("  {id}");
                }
                println!("marked_stopped: {} session(s)", result.marked_stopped.len());
                for id in &result.marked_stopped {
                    println!("  {id}");
                }
                println!("orphaned_tmux:  {} session(s)", result.orphaned_tmux.len());
                for n in &result.orphaned_tmux {
                    println!("  {n}");
                }
            }
        }
        "attach-cmd" => {
            if args.len() < 2 {
                die("tmux attach-cmd requires <session_name>");
            }
            let name = &args[1];
            let tmux_bin = match crate::tmux::resolve_tmux_bin() {
                Some(b) => b,
                None => die("no tmux binary available (RYVE_TMUX_PATH unset and not on PATH)"),
            };
            let ryve_dir = RyveDir::new(workshop_root);
            let client = crate::tmux::TmuxClient::new(tmux_bin, ryve_dir.root());
            let cmd = client.attach_command(name);
            let program = cmd.get_program().to_string_lossy().into_owned();
            let argv: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            if json_mode {
                let payload = serde_json::json!({
                    "program": program,
                    "args": argv,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            } else {
                print!("{program}");
                for a in &argv {
                    print!(" {a}");
                }
                println!();
            }
        }
        other => die(&format!(
            "unknown tmux subcommand '{other}' (reconcile, attach-cmd)"
        )),
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

// ── Release ─────────────────────────────────────────

async fn handle_release(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    args: &[String],
    json_mode: bool,
) {
    if args.is_empty() {
        die(
            "release subcommand required (create, list, show, edit, add-epic, remove-epic, status, close)",
        );
    }
    match args[0].as_str() {
        "create" => {
            if args.len() < 2 {
                die("release create requires <major|minor|patch>");
            }
            let bump = match args[1].as_str() {
                "major" => data::release_version::Bump::Major,
                "minor" => data::release_version::Bump::Minor,
                "patch" => data::release_version::Bump::Patch,
                other => die(&format!(
                    "invalid bump kind '{other}': expected major, minor, or patch"
                )),
            };

            // Find the maximum semver among closed releases to bump from.
            let closed = release_repo::list(pool, Some(vec![ReleaseStatus::Closed]))
                .await
                .unwrap_or_default();
            let prev = closed
                .iter()
                .filter_map(|r| data::release_version::parse(&r.version).ok())
                .max();
            let version = match data::release_version::next(prev, bump) {
                Ok(v) => data::release_version::format(v),
                Err(e) => die(&format!("version computation failed: {e}")),
            };

            let branch_name = data::release_branch::release_branch_name(&version);
            let new = NewRelease {
                version,
                branch_name: Some(branch_name),
                problem: None,
                acceptance: Vec::new(),
                notes: None,
            };
            match release_repo::create(pool, new).await {
                Ok(r) => {
                    // Cut the release branch so the DB row and the git
                    // branch are created atomically from the CLI's
                    // perspective.
                    let rb = data::release_branch::open(workshop_root);
                    match rb.cut_release_branch(&r.version).await {
                        Ok(branch) => {
                            if json_mode {
                                let payload = serde_json::json!({
                                    "release": r,
                                    "branch": branch,
                                });
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                                );
                            } else {
                                println!("created {} — v{} ({})", r.id, r.version, r.status);
                                println!("  branch: {branch}");
                            }
                        }
                        Err(e) => {
                            // The DB row was created but branch cutting
                            // failed. Report both so the user can retry
                            // the branch cut manually.
                            eprintln!("warning: release row created but branch cut failed: {e}");
                            if json_mode {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&r).unwrap_or_default()
                                );
                            } else {
                                println!("created {} — v{} ({})", r.id, r.version, r.status);
                            }
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => match release_repo::list(pool, None).await {
            Ok(releases) => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&releases).unwrap_or_default()
                    );
                } else if releases.is_empty() {
                    println!("No releases.");
                } else {
                    println!("{:<16} {:<12} {:<12} BRANCH", "ID", "VERSION", "STATUS");
                    let sep = "-".repeat(64);
                    println!("{sep}");
                    for r in &releases {
                        let branch = r.branch_name.as_deref().unwrap_or("");
                        println!("{:<16} {:<12} {:<12} {}", r.id, r.version, r.status, branch);
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        },
        "show" => {
            if args.len() < 2 {
                die("release show requires <id>");
            }
            let release = match release_repo::get(pool, &args[1]).await {
                Ok(r) => r,
                Err(e) => die(&format!("{e}")),
            };
            let epics = release_repo::list_member_epics(pool, &release.id)
                .await
                .unwrap_or_default();
            if json_mode {
                let payload = serde_json::json!({
                    "release": release,
                    "epics": epics,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            } else {
                println!("ID:       {}", release.id);
                println!("Version:  {}", release.version);
                println!("Status:   {}", release.status);
                if let Some(ref b) = release.branch_name {
                    println!("Branch:   {b}");
                }
                println!("Created:  {}", release.created_at);
                if let Some(ref t) = release.cut_at {
                    println!("Cut at:   {t}");
                }
                if let Some(ref t) = release.tag {
                    println!("Tag:      {t}");
                }
                if let Some(ref p) = release.artifact_path {
                    println!("Artifact: {p}");
                }
                if let Some(ref p) = release.problem {
                    println!("Problem:  {p}");
                }
                let acc = release.acceptance();
                if !acc.is_empty() {
                    println!("Acceptance:");
                    for a in &acc {
                        println!("  - {a}");
                    }
                }
                if let Some(ref n) = release.notes {
                    println!("Notes:    {n}");
                }
                if epics.is_empty() {
                    println!("\nNo member epics.");
                } else {
                    println!("\nMember epics ({}):", epics.len());
                    for eid in &epics {
                        println!("  {eid}");
                    }
                }
            }
        }
        "add-epic" => {
            if args.len() < 3 {
                die("release add-epic requires <release_id> <epic_id>");
            }
            match release_repo::add_epic(pool, &args[1], &args[2]).await {
                Ok(()) => {
                    if json_mode {
                        let payload = serde_json::json!({
                            "release_id": args[1],
                            "epic_id": args[2],
                            "action": "added",
                        });
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&payload).unwrap_or_default()
                        );
                    } else {
                        println!("{} added to release {}", args[2], args[1]);
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "remove-epic" => {
            if args.len() < 3 {
                die("release remove-epic requires <release_id> <epic_id>");
            }
            match release_repo::remove_epic(pool, &args[1], &args[2]).await {
                Ok(()) => {
                    if json_mode {
                        let payload = serde_json::json!({
                            "release_id": args[1],
                            "epic_id": args[2],
                            "action": "removed",
                        });
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&payload).unwrap_or_default()
                        );
                    } else {
                        println!("{} removed from release {}", args[2], args[1]);
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "status" => {
            if args.len() < 3 {
                die("release status requires <id> <new_status>");
            }
            let status = match ReleaseStatus::from_str(&args[2]) {
                Some(s) => s,
                None => die(&format!(
                    "invalid release status '{}': expected planning, in_progress, ready, cut, closed, or abandoned",
                    args[2]
                )),
            };
            match release_repo::set_status(pool, &args[1], status).await {
                Ok(r) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
                    } else {
                        println!("{} -> {}", r.id, r.status);
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "edit" => {
            if args.len() < 2 {
                die(
                    "release edit requires <id> [--version <ver>] [--notes <text>] [--clear-notes] [--problem <text>] [--clear-problem] [--branch <name>] [--clear-branch]",
                );
            }
            let release_id = &args[1];
            let mut patch = UpdateRelease::default();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--version" => {
                        i += 1;
                        if i >= args.len() {
                            die("--version requires a value");
                        }
                        patch.version = Some(args[i].clone());
                    }
                    "--notes" => {
                        i += 1;
                        if i >= args.len() {
                            die("--notes requires a value");
                        }
                        patch.notes = Some(Some(args[i].clone()));
                    }
                    "--clear-notes" => {
                        patch.notes = Some(None);
                    }
                    "--problem" => {
                        i += 1;
                        if i >= args.len() {
                            die("--problem requires a value");
                        }
                        patch.problem = Some(Some(args[i].clone()));
                    }
                    "--clear-problem" => {
                        patch.problem = Some(None);
                    }
                    "--branch" => {
                        i += 1;
                        if i >= args.len() {
                            die("--branch requires a value");
                        }
                        // Reject values that aren't valid git branch names up
                        // front (whitespace, "..", "@{", control chars, lock
                        // suffixes, etc). The release row's branch_name is
                        // surfaced everywhere as a real git ref; persisting
                        // garbage here just defers the failure to the next
                        // git operation that consumes it. Reuses the
                        // workshop-side validator so the rule lives in one
                        // place.
                        if let Err(err) = crate::workshop::validate_git_branch_name(&args[i]) {
                            die(&format!("--branch '{}' rejected: {err}", args[i]));
                        }
                        patch.branch_name = Some(Some(args[i].clone()));
                    }
                    "--clear-branch" => {
                        patch.branch_name = Some(None);
                    }
                    other => die(&format!("unknown flag '{other}' for release edit")),
                }
                i += 1;
            }
            match release_repo::update(pool, release_id, patch).await {
                Ok(r) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
                    } else {
                        println!("{} updated — v{} ({})", r.id, r.version, r.status);
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "close" => {
            if args.len() < 2 {
                die("release close requires <id>");
            }
            release_close(pool, workshop_root, &args[1], json_mode).await;
        }
        other => die(&format!(
            "unknown release subcommand '{other}': expected create, list, show, edit, add-epic, remove-epic, status, or close"
        )),
    }
}

/// Orchestrate the full release-close ceremony:
///   1. Verify all member epics are closed
///   2. Tag the release branch
///   3. Build the artifact
///   4. Record the artifact path on the release row
///   5. Transition to closed
///
/// On intermediate failure, rollback is best-effort: we do NOT leave a
/// half-closed release in the DB. The release stays in its current status and
/// the caller gets a non-zero exit code.
async fn release_close(
    pool: &sqlx::SqlitePool,
    workshop_root: &Path,
    release_id: &str,
    json_mode: bool,
) {
    // 0. Fetch the release.
    let release = match release_repo::get(pool, release_id).await {
        Ok(r) => r,
        Err(e) => die(&format!("{e}")),
    };

    // 1. Verify all member epics are closed.
    let epics = release_repo::list_member_epics(pool, release_id)
        .await
        .unwrap_or_default();
    let mut unclosed: Vec<String> = Vec::new();
    for eid in &epics {
        match spark_repo::get(pool, eid).await {
            Ok(spark) => {
                if spark.status != "closed" {
                    unclosed.push(format!("{} (status: {})", spark.id, spark.status));
                }
            }
            Err(e) => die(&format!("failed to fetch epic {eid}: {e}")),
        }
    }
    if !unclosed.is_empty() {
        die(&format!(
            "cannot close release: {} unclosed epic(s): {}",
            unclosed.len(),
            unclosed.join(", ")
        ));
    }

    // 2. Checkout the release branch so tag_release finds HEAD on the
    //    expected branch. We restore the original branch afterwards.
    let rb = data::release_branch::open(workshop_root);
    let original_branch = {
        let out = tokio::process::Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(workshop_root)
            .output()
            .await;
        out.ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let release_branch = data::release_branch::release_branch_name(&release.version);
    let checkout_out = tokio::process::Command::new("git")
        .args(["checkout", &release_branch])
        .current_dir(workshop_root)
        .output()
        .await;
    match checkout_out {
        Ok(o) if !o.status.success() => {
            die(&format!(
                "failed to checkout release branch {release_branch}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        Err(e) => die(&format!("failed to checkout release branch: {e}")),
        _ => {}
    }

    let tag_name = format!("v{}", release.version);
    // We need an artifact path for the tag message — use the deterministic
    // path even though the artifact doesn't exist yet. The tag message is
    // informational and the path will be correct once step 3 completes.
    let target_triple = match crate::release_artifact::host_target_triple_for_cli().await {
        Ok(t) => t,
        Err(e) => die(&format!("failed to determine host target triple: {e}")),
    };
    let anticipated_artifact =
        crate::release_artifact::artifact_path_for(workshop_root, &release.version, &target_triple);
    if let Err(e) = rb
        .tag_release(&release.version, &anticipated_artifact)
        .await
    {
        die(&format!("failed to tag release: {e}"));
    }

    // 3. Build the artifact.
    let artifact_path = match crate::release_artifact::build_release_artifact(
        workshop_root,
        &release.version,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            // Rollback: delete the tag we just created.
            let _ = tokio::process::Command::new("git")
                .args(["tag", "-d", &tag_name])
                .current_dir(workshop_root)
                .output()
                .await;
            die(&format!("artifact build failed (tag rolled back): {e}"));
        }
    };

    // 4. Record metadata on the release row.
    if let Err(e) = release_repo::record_close_metadata(
        pool,
        release_id,
        &tag_name,
        &artifact_path.to_string_lossy(),
    )
    .await
    {
        // Rollback: delete tag, remove artifact.
        let _ = tokio::process::Command::new("git")
            .args(["tag", "-d", &tag_name])
            .current_dir(workshop_root)
            .output()
            .await;
        let _ = tokio::fs::remove_file(&artifact_path).await;
        die(&format!(
            "failed to record close metadata (tag + artifact rolled back): {e}"
        ));
    }

    // 5. Transition to closed.
    match release_repo::set_status(pool, release_id, ReleaseStatus::Closed).await {
        Ok(r) => {
            if json_mode {
                let payload = serde_json::json!({
                    "release": r,
                    "tag": tag_name,
                    "artifact_path": artifact_path.to_string_lossy(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            } else {
                println!("release {} closed", r.id);
                println!("  tag:      {tag_name}");
                println!("  artifact: {}", artifact_path.display());
            }
        }
        Err(e) => {
            // Rollback: clear metadata, delete tag, remove artifact.
            let _ =
                sqlx::query("UPDATE releases SET tag = NULL, artifact_path = NULL WHERE id = ?")
                    .bind(release_id)
                    .execute(pool)
                    .await;
            let _ = tokio::process::Command::new("git")
                .args(["tag", "-d", &tag_name])
                .current_dir(workshop_root)
                .output()
                .await;
            let _ = tokio::fs::remove_file(&artifact_path).await;
            die(&format!(
                "failed to transition to closed (tag + artifact + metadata rolled back): {e}"
            ));
        }
    }

    // 6. Restore the original branch so the working tree is back where
    //    the user started.
    if let Some(ref branch) = original_branch {
        let _ = tokio::process::Command::new("git")
            .args(["checkout", branch])
            .current_dir(workshop_root)
            .output()
            .await;
    }
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
