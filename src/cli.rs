// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! `ryve-cli` — command-line interface for workgraph operations.
//!
//! Designed for use by Hands (coding agents) and humans from the terminal.
//! Operates on the `.ryve/sparks.db` in the current directory.
//!
//! Supports `--json` flag on most commands for machine-parseable output.

use std::path::PathBuf;
use std::process;

use data::ryve_dir::RyveDir;
use data::sparks::types::*;
use data::sparks::{
    assignment_repo, bond_repo, comment_repo, commit_link_repo, constraint_helpers, contract_repo,
    ember_repo, event_repo, spark_repo, stamp_repo,
};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

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

    if matches!(args_clean.get(1).map(|s| s.as_str()), Some("help" | "--help" | "-h")) {
        print_usage();
        return;
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ryve_dir = RyveDir::new(&cwd);

    // Special: `init` doesn't need an existing DB
    if args_clean.get(1).map(|s| s.as_str()) == Some("init") {
        handle_init(&ryve_dir, &cwd).await;
        return;
    }

    if !ryve_dir.sparks_db_path().exists() {
        die("no .ryve/sparks.db found in current directory. Run `ryve-cli init` or use a Ryve workshop.");
    }

    let pool = match data::db::open_sparks_db(&cwd).await {
        Ok(p) => p,
        Err(e) => die(&format!("failed to open database: {e}")),
    };

    let ws_id = cwd
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    match args_clean[1].as_str() {
        "spark" | "sparks" => handle_spark(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "bond" | "bonds" => handle_bond(&pool, &args_clean[2..], json_mode).await,
        "comment" | "comments" => handle_comment(&pool, &args_clean[2..], json_mode).await,
        "stamp" | "stamps" => handle_stamp(&pool, &args_clean[2..]).await,
        "contract" | "contracts" => handle_contract(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "constraint" | "constraints" => handle_constraint(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "ember" | "embers" => handle_ember(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "event" | "events" => handle_event(&pool, &args_clean[2..], json_mode).await,
        "assign" | "assignment" => handle_assignment(&pool, &args_clean[2..], json_mode).await,
        "commit" | "commits" => handle_commit(&pool, &args_clean[2..], &ws_id, json_mode).await,
        "hot" => handle_hot(&pool, &ws_id, json_mode).await,
        "status" => handle_status(&pool, &ws_id).await,
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

fn print_usage() {
    eprintln!("ryve-cli — workgraph operations for Ryve workshops\n");
    eprintln!("USAGE: ryve-cli [--json] <command> <subcommand> [args...]\n");
    eprintln!("COMMANDS:");
    eprintln!("  init                                Initialize .ryve/ in current directory");
    eprintln!("  status                              Show workshop summary");
    eprintln!("  hot                                 List hot (ready-to-work) sparks");
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
    eprintln!("FLAGS:");
    eprintln!("  --json    Output as JSON (for machine consumption)");
    eprintln!();
    eprintln!("Run from a Ryve workshop root (directory containing .ryve/).");
}

// ── Init ─────────────────────────────────────────────

async fn handle_init(ryve_dir: &RyveDir, cwd: &PathBuf) {
    if let Err(e) = data::ryve_dir::init_ryve_dir(ryve_dir).await {
        die(&format!("failed to initialize: {e}"));
    }
    if let Err(e) = data::db::open_sparks_db(cwd).await {
        die(&format!("failed to create database: {e}"));
    }
    println!("initialized .ryve/ in {}", cwd.display());
}

// ── Status ───────────────────────────────────────────

async fn handle_status(pool: &sqlx::SqlitePool, ws_id: &str) {
    let all = spark_repo::list(pool, SparkFilter::default()).await.unwrap_or_default();
    let open = all.iter().filter(|s| s.status == "open").count();
    let in_progress = all.iter().filter(|s| s.status == "in_progress").count();
    let blocked = all.iter().filter(|s| s.status == "blocked").count();
    let closed = all.iter().filter(|s| s.status == "closed").count();

    let failing = contract_repo::list_failing(pool, ws_id).await.unwrap_or_default();
    let constraints = constraint_helpers::list(pool, ws_id).await.unwrap_or_default();

    println!("Workshop: {ws_id}");
    println!("Sparks:   {} open, {} in progress, {} blocked, {} closed ({} total)",
        open, in_progress, blocked, closed, all.len());
    println!("Contracts: {} failing/pending", failing.len());
    println!("Constraints: {} defined", constraints.len());
}

// ── Hot ──────────────────────────────────────────────

async fn handle_hot(pool: &sqlx::SqlitePool, ws_id: &str, json_mode: bool) {
    match data::sparks::graph::hot_sparks(pool, ws_id).await {
        Ok(sparks) => {
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&sparks).unwrap_or_default());
            } else if sparks.is_empty() {
                println!("No hot sparks (all blocked, deferred, or closed).");
            } else {
                println!("{:<8} {:<3} {:<12} {}", "ID", "P", "TYPE", "TITLE");
                println!("{}", "-".repeat(60));
                for s in &sparks {
                    println!("{:<8} P{:<1} {:<12} {}", s.id, s.priority, s.spark_type, s.title);
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
                    status: Some(vec![SparkStatus::Open, SparkStatus::InProgress, SparkStatus::Blocked]),
                    ..Default::default()
                }
            };
            match spark_repo::list(pool, filter).await {
                Ok(sparks) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&sparks).unwrap_or_default());
                    } else if sparks.is_empty() {
                        println!("No sparks found.");
                    } else {
                        println!("{:<8} {:<3} {:<8} {:<12} {:<12} {}", "ID", "P", "RISK", "TYPE", "STATUS", "TITLE");
                        println!("{}", "-".repeat(72));
                        for s in &sparks {
                            let risk = s.risk_level.as_deref().unwrap_or("normal");
                            println!("{:<8} P{:<1} {:<8} {:<12} {:<12} {}", s.id, s.priority, risk, s.spark_type, s.status, s.title);
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
                eprintln!("  --type, -t <type>           bug|feature|task|epic|chore|spike|milestone (default: task)");
                eprintln!("  --priority, -p <0-4>        P0=critical, P4=negligible (default: 2)");
                eprintln!("  --risk, -r <level>          trivial|normal|elevated|critical");
                eprintln!("  --scope, -s <boundary>      Scope boundary (e.g. 'src/auth/')");
                eprintln!("  --description, -d <text>    Description");
                eprintln!("  --problem <text>            Intent: problem being solved");
                eprintln!("  --invariant <text>          Intent: invariant to preserve (repeatable)");
                eprintln!("  --non-goal <text>           Intent: non-goal (repeatable)");
                eprintln!("  --acceptance <text>         Intent: acceptance criterion (repeatable)");
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
                    "--type" | "-t" => { i += 1; if i < args.len() { spark_type = parse_spark_type(&args[i]); } }
                    "--priority" | "-p" => { i += 1; if i < args.len() { priority = args[i].parse().unwrap_or(2); } }
                    "--risk" | "-r" => { i += 1; if i < args.len() { risk = Some(parse_risk_level(&args[i])); } }
                    "--scope" | "-s" => { i += 1; if i < args.len() { scope = Some(args[i].clone()); } }
                    "--description" | "-d" => { i += 1; if i < args.len() { description = args[i].clone(); } }
                    "--problem" => { i += 1; if i < args.len() { problem = Some(args[i].clone()); } }
                    "--invariant" => { i += 1; if i < args.len() { invariants.push(args[i].clone()); } }
                    "--non-goal" => { i += 1; if i < args.len() { non_goals.push(args[i].clone()); } }
                    "--acceptance" => { i += 1; if i < args.len() { acceptance.push(args[i].clone()); } }
                    _ => title_parts.push(&args[i]),
                }
                i += 1;
            }
            let title = title_parts.join(" ");
            if title.is_empty() {
                die("spark create requires a title. Use `spark create --help` for options.");
            }

            // Build metadata JSON with intent if any intent fields provided
            let metadata = if problem.is_some() || !invariants.is_empty() || !non_goals.is_empty() || !acceptance.is_empty() {
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
                        println!("{}", serde_json::to_string_pretty(&spark).unwrap_or_default());
                    } else {
                        println!("created {} — {}", spark.id, title);
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "show" => {
            if args.len() < 2 { die("spark show requires <id>"); }
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
                        println!("Risk:        {}", s.risk_level.as_deref().unwrap_or("normal"));
                        if let Some(ref v) = s.scope_boundary { println!("Scope:       {v}"); }
                        if !s.description.is_empty() { println!("Description: {}", s.description); }
                        if let Some(ref a) = s.assignee { println!("Assignee:    {a}"); }
                        println!("Created:     {}", s.created_at);
                        println!("Updated:     {}", s.updated_at);
                        if let Some(ref c) = s.closed_at {
                            println!("Closed:      {c}");
                            println!("Reason:      {}", s.closed_reason.as_deref().unwrap_or(""));
                        }
                        let intent = s.intent();
                        if let Some(ref p) = intent.problem_statement { println!("\nProblem:     {p}"); }
                        if !intent.invariants.is_empty() {
                            println!("Invariants:"); for inv in &intent.invariants { println!("  - {inv}"); }
                        }
                        if !intent.non_goals.is_empty() {
                            println!("Non-goals:"); for ng in &intent.non_goals { println!("  - {ng}"); }
                        }
                        if !intent.acceptance_criteria.is_empty() {
                            println!("Acceptance:"); for ac in &intent.acceptance_criteria { println!("  - {ac}"); }
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "status" => {
            if args.len() < 3 { die("spark status requires <id> <new_status>"); }
            let status = SparkStatus::from_str(&args[2])
                .unwrap_or_else(|| die(&format!("invalid status '{}' (open, in_progress, blocked, deferred, closed)", args[2])));
            let upd = UpdateSpark { status: Some(status), ..Default::default() };
            match spark_repo::update(pool, &args[1], upd, "cli").await {
                Ok(s) => println!("{} -> {}", s.id, s.status),
                Err(e) => die(&format!("{e}")),
            }
        }
        "close" => {
            if args.len() < 2 { die("spark close requires <id>"); }
            let reason = if args.len() > 2 { args[2..].join(" ") } else { "completed".to_string() };
            match spark_repo::close(pool, &args[1], &reason, "cli").await {
                Ok(s) => println!("{} closed — {reason}", s.id),
                Err(e) => die(&format!("{e}")),
            }
        }
        "edit" => {
            if args.len() < 2 { die("spark edit requires <id>"); }
            let id = &args[1];
            let mut upd = UpdateSpark::default();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--title" => { i += 1; if i < args.len() { upd.title = Some(args[i].clone()); } }
                    "--priority" => { i += 1; if i < args.len() { upd.priority = Some(args[i].parse().unwrap_or(2)); } }
                    "--risk" => { i += 1; if i < args.len() { upd.risk_level = Some(parse_risk_level(&args[i])); } }
                    "--scope" => { i += 1; if i < args.len() { upd.scope_boundary = Some(Some(args[i].clone())); } }
                    "--type" => { i += 1; if i < args.len() { upd.spark_type = Some(parse_spark_type(&args[i])); } }
                    "--description" => { i += 1; if i < args.len() { upd.description = Some(args[i].clone()); } }
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
    if args.is_empty() { die("bond subcommand required (create, list, delete)"); }
    match args[0].as_str() {
        "create" => {
            if args.len() < 4 { die("bond create requires <from_id> <to_id> <type>"); }
            let bond_type = parse_bond_type(&args[3]);
            match bond_repo::create(pool, &args[1], &args[2], bond_type).await {
                Ok(b) => println!("bond {} created: {} --[{}]--> {}", b.id, b.from_id, b.bond_type, b.to_id),
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            if args.len() < 2 { die("bond list requires <spark_id>"); }
            match bond_repo::list_for_spark(pool, &args[1]).await {
                Ok(bonds) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&bonds).unwrap_or_default());
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
            if args.len() < 2 { die("bond delete requires <id>"); }
            let id: i64 = args[1].parse().unwrap_or_else(|_| die("bond id must be a number"));
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
    if args.is_empty() { die("comment subcommand required (add, list)"); }
    match args[0].as_str() {
        "add" => {
            if args.len() < 3 { die("comment add requires <spark_id> <body>"); }
            let body = args[2..].join(" ");
            let new = NewComment { spark_id: args[1].clone(), author: "cli".to_string(), body: body.clone() };
            match comment_repo::create(pool, new).await {
                Ok(c) => println!("{} added to {}", c.id, args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            if args.len() < 2 { die("comment list requires <spark_id>"); }
            match comment_repo::list_for_spark(pool, &args[1]).await {
                Ok(comments) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&comments).unwrap_or_default());
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
    if args.is_empty() { die("stamp subcommand required (add, remove, list)"); }
    match args[0].as_str() {
        "add" => {
            if args.len() < 3 { die("stamp add requires <spark_id> <label>"); }
            match stamp_repo::add(pool, &args[1], &args[2]).await {
                Ok(()) => println!("stamp '{}' added to {}", args[2], args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "remove" => {
            if args.len() < 3 { die("stamp remove requires <spark_id> <label>"); }
            match stamp_repo::remove(pool, &args[1], &args[2]).await {
                Ok(()) => println!("stamp '{}' removed from {}", args[2], args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            if args.len() < 2 { die("stamp list requires <spark_id>"); }
            match stamp_repo::list_for_spark(pool, &args[1]).await {
                Ok(stamps) => {
                    if stamps.is_empty() { println!("No stamps on {}.", args[1]); }
                    else { println!("{}", stamps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")); }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown stamp subcommand '{other}'")),
    }
}

// ── Contract ─────────────────────────────────────────

async fn handle_contract(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() { die("contract subcommand required (list, add, check, failing)"); }
    match args[0].as_str() {
        "list" | "ls" => {
            if args.len() < 2 { die("contract list requires <spark_id>"); }
            match contract_repo::list_for_spark(pool, &args[1]).await {
                Ok(contracts) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&contracts).unwrap_or_default());
                    } else if contracts.is_empty() {
                        println!("No contracts for {}.", args[1]);
                    } else {
                        println!("{:<4} {:<14} {:<10} {:<8} {}", "ID", "KIND", "ENFORCE", "STATUS", "DESCRIPTION");
                        println!("{}", "-".repeat(65));
                        for c in &contracts { println!("{:<4} {:<14} {:<10} {:<8} {}", c.id, c.kind, c.enforcement, c.status, c.description); }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "add" => {
            if args.len() < 4 { die("contract add requires <spark_id> <kind> <description>"); }
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
            if args.len() < 3 { die("contract check requires <contract_id> <pass|fail|skip>"); }
            let id: i64 = args[1].parse().unwrap_or_else(|_| die("contract id must be a number"));
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
        "failing" => {
            match contract_repo::list_failing(pool, ws_id).await {
                Ok(contracts) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&contracts).unwrap_or_default());
                    } else if contracts.is_empty() {
                        println!("No failing contracts.");
                    } else {
                        for c in &contracts {
                            println!("[{}] {} on {} — {}", c.status.to_uppercase(), c.kind, c.spark_id, c.description);
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown contract subcommand '{other}'")),
    }
}

// ── Constraint ───────────────────────────────────────

async fn handle_constraint(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() || args[0] == "list" || args[0] == "ls" {
        match constraint_helpers::list(pool, ws_id).await {
            Ok(constraints) => {
                if json_mode {
                    let map: Vec<_> = constraints.iter().map(|(n, c)| serde_json::json!({"name": n, "constraint": c})).collect();
                    println!("{}", serde_json::to_string_pretty(&map).unwrap_or_default());
                } else if constraints.is_empty() {
                    println!("No architectural constraints defined.");
                } else {
                    for (name, c) in &constraints {
                        let sev = match c.severity { ConstraintSeverity::Error => "ERROR", ConstraintSeverity::Warning => "WARN", ConstraintSeverity::Info => "INFO" };
                        println!("[{sev}] {name} — {}", c.rule);
                        if let Some(ref r) = c.rationale { println!("  rationale: {r}"); }
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        }
    }
}

// ── Ember ────────────────────────────────────────────

async fn handle_ember(pool: &sqlx::SqlitePool, args: &[String], ws_id: &str, json_mode: bool) {
    if args.is_empty() { die("ember subcommand required (list, send, sweep)"); }
    match args[0].as_str() {
        "list" | "ls" => {
            match ember_repo::list_active(pool, ws_id).await {
                Ok(embers) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&embers).unwrap_or_default());
                    } else if embers.is_empty() {
                        println!("No active embers.");
                    } else {
                        for e in &embers {
                            println!("[{}] {} (from: {}, ttl: {}s): {}", e.id, e.ember_type, e.source_agent.as_deref().unwrap_or("?"), e.ttl_seconds, e.content);
                        }
                    }
                }
                Err(e) => die(&format!("{e}")),
            }
        }
        "send" => {
            if args.len() < 3 { die("ember send requires <type> <content>"); }
            let ember_type = match args[1].as_str() {
                "glow" => EmberType::Glow, "flash" => EmberType::Flash, "flare" => EmberType::Flare,
                "blaze" => EmberType::Blaze, "ash" => EmberType::Ash,
                other => die(&format!("invalid ember type '{other}' (glow, flash, flare, blaze, ash)")),
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
        "sweep" => {
            match ember_repo::sweep_expired(pool).await {
                Ok(count) => println!("{count} expired embers cleaned up"),
                Err(e) => die(&format!("{e}")),
            }
        }
        other => die(&format!("unknown ember subcommand '{other}'")),
    }
}

// ── Event ────────────────────────────────────────────

async fn handle_event(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() || args[0] == "list" || args[0] == "ls" {
        if args.len() < 2 { die("event list requires <spark_id>"); }
        let spark_id = if args[0] == "list" || args[0] == "ls" { &args[1] } else { &args[0] };
        match event_repo::list_for_spark(pool, spark_id).await {
            Ok(events) => {
                if json_mode {
                    println!("{}", serde_json::to_string_pretty(&events).unwrap_or_default());
                } else if events.is_empty() {
                    println!("No events for {spark_id}.");
                } else {
                    for e in &events {
                        let old = e.old_value.as_deref().unwrap_or("null");
                        let new = e.new_value.as_deref().unwrap_or("null");
                        let actor_type = e.actor_type.as_deref().unwrap_or("");
                        println!("[{}] {} ({}): {} {} -> {}", e.timestamp, e.actor, actor_type, e.field_name, old, new);
                    }
                }
            }
            Err(e) => die(&format!("{e}")),
        }
    }
}

// ── Assignment ───────────────────────────────────────

async fn handle_assignment(pool: &sqlx::SqlitePool, args: &[String], json_mode: bool) {
    if args.is_empty() { die("assign subcommand required (claim, release, list)"); }
    match args[0].as_str() {
        "claim" => {
            if args.len() < 3 { die("assign claim requires <session_id> <spark_id>"); }
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
            if args.len() < 3 { die("assign release requires <session_id> <spark_id>"); }
            match assignment_repo::abandon(pool, &args[1], &args[2]).await {
                Ok(()) => println!("{} released by {}", args[2], args[1]),
                Err(e) => die(&format!("{e}")),
            }
        }
        "list" | "ls" => {
            if args.len() < 2 { die("assign list requires <spark_id>"); }
            match assignment_repo::active_for_spark(pool, &args[1]).await {
                Ok(Some(a)) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&a).unwrap_or_default());
                    } else {
                        println!("{} owned by {} (since {})", a.spark_id, a.session_id, a.assigned_at);
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
    if args.is_empty() { die("commit subcommand required (link, list, scan)"); }
    match args[0].as_str() {
        "link" => {
            if args.len() < 3 { die("commit link requires <spark_id> <hash>"); }
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
            if args.len() < 2 { die("commit list requires <spark_id>"); }
            match commit_link_repo::list_for_spark(pool, &args[1]).await {
                Ok(links) => {
                    if json_mode {
                        println!("{}", serde_json::to_string_pretty(&links).unwrap_or_default());
                    } else if links.is_empty() {
                        println!("No commits linked to {}.", args[1]);
                    } else {
                        for l in &links {
                            let msg = l.commit_message.as_deref().unwrap_or("");
                            println!("{} ({}) {}", &l.commit_hash[..8.min(l.commit_hash.len())], l.linked_by, msg);
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
                            println!("{} [{}] {} — {}", &r.hash[..8], r.spark_ids.join(", "), r.author, r.message);
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
        "bug" => SparkType::Bug, "feature" => SparkType::Feature, "task" => SparkType::Task,
        "epic" => SparkType::Epic, "chore" => SparkType::Chore, "spike" => SparkType::Spike,
        "milestone" => SparkType::Milestone,
        _ => { eprintln!("warning: unknown type '{s}', defaulting to task"); SparkType::Task }
    }
}

fn parse_risk_level(s: &str) -> RiskLevel {
    match s {
        "trivial" => RiskLevel::Trivial, "normal" => RiskLevel::Normal,
        "elevated" => RiskLevel::Elevated, "critical" => RiskLevel::Critical,
        _ => { eprintln!("warning: unknown risk '{s}', defaulting to normal"); RiskLevel::Normal }
    }
}

fn parse_bond_type(s: &str) -> BondType {
    match s {
        "blocks" => BondType::Blocks, "parent_child" => BondType::ParentChild,
        "related" => BondType::Related, "conditional_blocks" => BondType::ConditionalBlocks,
        "waits_for" => BondType::WaitsFor, "duplicates" => BondType::Duplicates,
        "supersedes" => BondType::Supersedes,
        _ => die(&format!("invalid bond type '{s}' (blocks, parent_child, related, conditional_blocks, waits_for, duplicates, supersedes)")),
    }
}

fn parse_contract_kind(s: &str) -> ContractKind {
    match s {
        "test_pass" => ContractKind::TestPass, "no_api_break" => ContractKind::NoApiBreak,
        "custom_command" => ContractKind::CustomCommand, "grep_absent" => ContractKind::GrepAbsent,
        "grep_present" => ContractKind::GrepPresent,
        _ => die(&format!("invalid contract kind '{s}' (test_pass, no_api_break, custom_command, grep_absent, grep_present)")),
    }
}
