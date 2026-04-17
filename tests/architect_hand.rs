// SPDX-License-Identifier: AGPL-3.0-or-later

//! End-to-end integration test for the Architect Hand archetype
//! ([sp-1471f46a] / spark ryve-3f799949).
//!
//! Drives the full spawn path against a **synthetic multi-language**
//! workshop (a Python module + a TypeScript module living in the same
//! repo), then asserts the Architect contract from the spark intent:
//!
//!   1. Architect archetype exists in the registry (here: `HandKind::
//!      Architect` accessible via `ryve hand spawn --role architect`).
//!   2. Capability class is Reviewer/Cartographer and the tool policy
//!      is read-only — verified by asserting that no source files in
//!      the worktree are mutated across the spawn + simulated review.
//!   3. Outputs are structured comments on the parent spark
//!      (recommendations, tradeoffs, risks) rather than diffs.
//!   4. The prompt embedded in `.ryve/prompts/hand-<session>.md` carries
//!      the read-only discipline, the `ryve comment add` channel, the
//!      RECOMMENDATION schema, the language-neutral category vocabulary,
//!      and the audit spark's problem statement.
//!   5. Architects run against a multi-language project without
//!      language-specific tuning: the synthetic repo contains both a
//!      `.py` and a `.ts` file; we assert the Architect's spawn-time
//!      prompt references neither extension in its hard-coded text (the
//!      prompt must be language-neutral — same invariant checked by the
//!      unit tests in `agent_prompts.rs`).
//!
//! The test is self-contained: its own tempdir, its own sparks.db pool,
//! its own stub agent. It uses the stub agent rather than a real
//! claude/codex so no network calls are made. The test gates on a
//! tmux binary (bundled or system) and skips cleanly if none is
//! available, so CI runners without tmux stay green.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Mirror of `src/tmux.rs::resolve_tmux_bin` (same rationale as
/// `tests/investigator_hand.rs` — the test binary has no direct
/// dependency on the bin crate's internals).
fn find_tmux_binary() -> Option<PathBuf> {
    for var in ["RYVE_TMUX_PATH", "RYVE_TMUX_BIN"] {
        if let Ok(val) = std::env::var(var) {
            let p = PathBuf::from(val);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bundled = manifest.join("vendor/tmux/bin/tmux");
    if bundled.exists() {
        return Some(bundled);
    }
    let out = Command::new("which").arg("tmux").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Build a throwaway workshop: `git init`, an empty commit, `ryve init`,
/// and a **multi-language** source tree (one Python module, one TypeScript
/// module) so the test exercises the spark's "synthetic multi-language
/// project" acceptance criterion. Returns the workshop root, the path to
/// the stub agent, and the two source-file paths so the test can assert
/// their contents are unchanged after the spawn.
fn setup_multilang_workshop() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ryve-architect-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create workshop tempdir");

    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&root)
            .env("GIT_AUTHOR_NAME", "ryve-test")
            .env("GIT_AUTHOR_EMAIL", "test@ryve.local")
            .env("GIT_COMMITTER_NAME", "ryve-test")
            .env("GIT_COMMITTER_EMAIL", "test@ryve.local")
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed in {root:?}");
    };
    run_git(&["init", "-q", "-b", "main"]);
    run_git(&["config", "commit.gpgsign", "false"]);

    // Synthetic source tree: Python + TypeScript. The contents are
    // deliberately small and generic — a module that wraps a queue and
    // a module that reads from it — so a plausible "review this
    // boundary" Architect spark has something to cite. The Architect
    // under test never reads these files (the stub agent doesn't
    // inspect anything), but the files must EXIST so the negative
    // assertion "no source file was mutated" is meaningful.
    let py_dir = root.join("services/ingest");
    std::fs::create_dir_all(&py_dir).expect("create python dir");
    let py_path = py_dir.join("pipeline.py");
    std::fs::write(
        &py_path,
        "# services/ingest/pipeline.py\n\
         def enqueue(event):\n\
         \x20\x20\x20\x20# TODO: bound this queue\n\
         \x20\x20\x20\x20_queue.append(event)\n\
         \n\
         _queue = []\n",
    )
    .expect("write python source");

    let ts_dir = root.join("services/projection");
    std::fs::create_dir_all(&ts_dir).expect("create ts dir");
    let ts_path = ts_dir.join("reader.ts");
    std::fs::write(
        &ts_path,
        "// services/projection/reader.ts\n\
         export function drain(queue: unknown[]): unknown[] {\n\
         \x20\x20const out = queue.slice();\n\
         \x20\x20queue.length = 0;\n\
         \x20\x20return out;\n\
         }\n",
    )
    .expect("write ts source");

    run_git(&["add", "."]);
    run_git(&["commit", "-q", "-m", "init: synthetic py+ts project"]);

    let status = Command::new(ryve_bin())
        .arg("init")
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .status()
        .expect("spawn ryve init");
    assert!(status.success(), "ryve init failed");

    // Stub agent: just sleeps so the tmux session stays alive long
    // enough for pipe-pane to attach. The Architect contract is that
    // the agent does NOT edit files, so the stub intentionally makes
    // zero filesystem writes outside of the tempdir it controls.
    let stub_path = root.join("stub-agent.sh");
    std::fs::write(&stub_path, "#!/bin/sh\nsleep 3\n").expect("write stub agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub_path, perms).unwrap();
    }

    (root, stub_path, py_path, ts_path)
}

fn ryve(root: &Path, tmux_bin: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ryve_bin())
        .args(args)
        .current_dir(root)
        .env("RYVE_WORKSHOP_ROOT", root)
        .env("RYVE_TMUX_PATH", tmux_bin)
        .output()
        .expect("spawn ryve")
}

fn kill_tmux_session(tmux_bin: &Path, root: &Path, session_name: &str) {
    let socket = expected_socket(root);
    let _ = Command::new(tmux_bin)
        .args([
            "-S",
            &socket.to_string_lossy(),
            "kill-session",
            "-t",
            session_name,
        ])
        .stderr(std::process::Stdio::null())
        .status();
}

fn expected_socket(workshop_dir: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let state_dir = workshop_dir.join(".ryve");
    let canonical = state_dir.join("tmux.sock");
    if canonical.to_string_lossy().len() <= 100 {
        return canonical;
    }
    let mut hasher = DefaultHasher::new();
    state_dir.hash(&mut hasher);
    let hash = hasher.finish();
    PathBuf::from(format!("/tmp/ryve-{hash:016x}.sock"))
}

fn parse_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let idx = json.find(&needle)?;
    let rest = &json[idx + needle.len()..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let quote_start = after.find('"')?;
    let after_quote = &after[quote_start + 1..];
    let quote_end = after_quote.find('"')?;
    Some(after_quote[..quote_end].to_string())
}

#[tokio::test]
async fn architect_hand_reviews_multilang_project_via_comments() {
    let Some(tmux_bin) = find_tmux_binary() else {
        eprintln!(
            "tmux binary not found — skipping Architect Hand integration test. \
             CI runners without bundled tmux are expected to hit this path."
        );
        return;
    };

    let (root, stub_path, py_path, ts_path) = setup_multilang_workshop();
    let py_before = std::fs::read_to_string(&py_path).expect("read py source");
    let ts_before = std::fs::read_to_string(&ts_path).expect("read ts source");

    // --- (1) Review spark: scope the Architect at the synthetic
    // Python+TS boundary. The problem statement is intentionally
    // generic (a shared mutable queue across a writer and a reader)
    // so the Architect prompt's language-neutral framing is what the
    // test exercises.
    let problem_statement = "Review the boundary between the ingest pipeline (services/ingest/pipeline.py) and the \
         projection reader (services/projection/reader.ts). Both modules share an unbounded \
         queue and we are seeing lost events under concurrent writes.";
    // The `ryve spark create` CLI requires `--parent <epic_id>` for any
    // non-epic spark. The Architect role is used on design-review work
    // that sits under a review or epic container; for the purposes of
    // this test we create the review spark as an epic so it can stand
    // alone in a fresh workshop (mirrors `tests/investigator_hand.rs`).
    let create = ryve(
        &root,
        &tmux_bin,
        &[
            "--json",
            "spark",
            "create",
            "--type",
            "epic",
            "--priority",
            "2",
            "--problem",
            problem_statement,
            "Architect review: ingest ↔ projection boundary",
        ],
    );
    assert!(
        create.status.success(),
        "spark create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let create_stdout = String::from_utf8_lossy(&create.stdout);
    let spark_id = parse_json_string(&create_stdout, "id")
        .unwrap_or_else(|| panic!("could not parse spark id from: {create_stdout}"));

    // --- (2) Spawn the Architect Hand. `hand spawn --role architect`
    // routes to `spawn_hand(HandKind::Architect)` inside the binary,
    // which is the registry entry under test.
    let spawn = ryve(
        &root,
        &tmux_bin,
        &[
            "--json",
            "hand",
            "spawn",
            &spark_id,
            "--role",
            "architect",
            "--agent",
            &stub_path.to_string_lossy(),
            "--actor",
            "tester",
        ],
    );
    assert!(
        spawn.status.success(),
        "hand spawn failed: stdout={} stderr={}",
        String::from_utf8_lossy(&spawn.stdout),
        String::from_utf8_lossy(&spawn.stderr)
    );
    let spawn_stdout = String::from_utf8_lossy(&spawn.stdout);
    let session_id = parse_json_string(&spawn_stdout, "session_id")
        .unwrap_or_else(|| panic!("could not parse session_id from: {spawn_stdout}"));

    // --- (3) Prompt-file assertions. The `spawn_hand` path writes the
    // composed prompt under `.ryve/prompts/hand-<session>.md` before
    // launching the agent. These assertions lock the Architect
    // contract at the spawn seam, independent of any specific coding
    // agent's behaviour.
    let prompt_path = root
        .join(".ryve")
        .join("prompts")
        .join(format!("hand-{session_id}.md"));
    let prompt = std::fs::read_to_string(&prompt_path).unwrap_or_else(|e| {
        panic!(
            "architect prompt file missing at {}: {e}",
            prompt_path.display()
        )
    });

    assert!(
        prompt.contains("Architect Hand"),
        "prompt must identify the Architect role; got:\n{prompt}"
    );
    assert!(
        prompt.contains("READ-ONLY"),
        "prompt must state the READ-ONLY contract; got:\n{prompt}"
    );
    assert!(
        prompt.contains("ryve comment add"),
        "prompt must direct recommendations to `ryve comment add`; got:\n{prompt}"
    );
    assert!(
        prompt.contains("RECOMMENDATION"),
        "prompt must include the RECOMMENDATION schema; got:\n{prompt}"
    );
    for field in ["tradeoffs", "risks", "alternatives"] {
        assert!(
            prompt.contains(field),
            "architect prompt must include the `{field}` recommendation field; got:\n{prompt}"
        );
    }
    // Language-neutrality of the SKELETON: the hard-coded prompt text
    // must not mention file extensions like `.py` or `.ts` or popular
    // framework names. (The problem_statement ABOVE does reference
    // `.py` / `.ts`; that's fine — it's spark-supplied content, not
    // skeleton text. To enforce the skeleton invariant we strip the
    // embedded problem_statement before scanning.)
    let skeleton = prompt.replace(problem_statement, "<PROBLEM>");
    for banned in [".py", ".ts", ".rs", "django", "fastapi", "react", "tokio"] {
        assert!(
            !skeleton.contains(banned),
            "architect prompt skeleton must be language-neutral; \
             found `{banned}`. Full skeleton:\n{skeleton}"
        );
    }
    // Problem-statement scoping: the prompt must embed enough of the
    // parent spark's problem_statement that the Architect knows what
    // it is reviewing. The investigator test uses 40 chars; mirror that.
    let ps_prefix: String = problem_statement.chars().take(40).collect();
    assert!(
        prompt.contains(&ps_prefix),
        "architect prompt must include the audit spark's problem statement \
         (first 40 chars: {ps_prefix:?}); got:\n{prompt}"
    );

    // --- (4) Session-row assertions. session_label must be "architect"
    // so the UI and post-mortem tools can distinguish Architect Hands
    // from other read-only archetypes.
    let pool = data::db::open_sparks_db(&root)
        .await
        .expect("open sparks.db for readback");
    let session = data::sparks::agent_session_repo::get(&pool, &session_id)
        .await
        .expect("session lookup")
        .unwrap_or_else(|| panic!("agent_sessions row missing for session {session_id}"));
    assert_eq!(
        session.session_label.as_deref(),
        Some("architect"),
        "session_label must be 'architect'"
    );

    // --- (5) Assignment-row assertions: Owner role (read-only
    // archetype still owns the audit spark for the lifetime of its
    // review).
    let assign = ryve(&root, &tmux_bin, &["--json", "assign", "list", &spark_id]);
    assert!(
        assign.status.success(),
        "assign list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&assign.stdout),
        String::from_utf8_lossy(&assign.stderr)
    );
    let assign_stdout = String::from_utf8_lossy(&assign.stdout);
    assert!(
        assign_stdout.contains(&session_id),
        "assignment must list the architect session: {assign_stdout}"
    );
    let role = parse_json_string(&assign_stdout, "role")
        .unwrap_or_else(|| panic!("no role field in: {assign_stdout}"));
    assert_eq!(
        role, "owner",
        "architect assignment role must be Owner (not Merger/Observer)"
    );

    // --- (6) Read-only invariant: NO source file under the worktree
    // has been mutated. This is the capability-gate enforcement check
    // from the spark's invariants. The stub agent never writes
    // anything; if a future change in `spawn_hand` accidentally gave
    // Architects a write-capable policy, downstream paths (e.g. an
    // overly enthusiastic "seed docs/adr/" hook) could silently
    // introduce writes. Guard both source files.
    let py_after = std::fs::read_to_string(&py_path).expect("re-read py source");
    let ts_after = std::fs::read_to_string(&ts_path).expect("re-read ts source");
    assert_eq!(
        py_before, py_after,
        "architect spawn must not mutate Python source"
    );
    assert_eq!(
        ts_before, ts_after,
        "architect spawn must not mutate TypeScript source"
    );

    // --- (7) Structured findings-as-comments flow. The Architect's
    // contract is that recommendations flow ONLY through
    // `ryve comment add`. Simulate one end-to-end and read it back via
    // `ryve comment list`, asserting the RECOMMENDATION schema (with
    // tradeoffs + risks — the fields that distinguish an Architect
    // recommendation from an Investigator finding) round-trips.
    let recommendation = r#"RECOMMENDATION
severity: high
category: boundary
location: services/ingest/pipeline.py:3, services/projection/reader.ts:2
recommendation: Introduce an explicit bounded queue module owned by the ingest service; the projection reader consumes via a public drain API rather than mutating the underlying list.
tradeoffs: one extra module boundary; slight throughput cost from the bound check on every enqueue.
risks: call sites reading _queue directly may be missed in the cut-over; mitigate with a grep-enforced lint against the private name.
alternatives: a pub/sub bus (rejected: pulls in an unnecessary dependency for a single-writer single-reader shape)."#;
    let add = ryve(
        &root,
        &tmux_bin,
        &["comment", "add", &spark_id, recommendation],
    );
    assert!(
        add.status.success(),
        "comment add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );
    let list = ryve(&root, &tmux_bin, &["--json", "comment", "list", &spark_id]);
    assert!(
        list.status.success(),
        "comment list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    let comments: serde_json::Value = serde_json::from_str(&list_stdout)
        .unwrap_or_else(|e| panic!("comment list returned invalid JSON ({e}): {list_stdout}"));
    let arr = comments
        .as_array()
        .unwrap_or_else(|| panic!("expected array from comment list: {list_stdout}"));
    assert!(
        arr.iter().any(|c| {
            c["spark_id"].as_str() == Some(&spark_id)
                && c["body"].as_str().is_some_and(|b| {
                    b.contains("RECOMMENDATION")
                        && b.contains("tradeoffs:")
                        && b.contains("risks:")
                        && b.contains("services/ingest/pipeline.py:3")
                })
        }),
        "architect recommendation comment must be persisted on spark {spark_id}: {list_stdout}"
    );

    // Cleanup. Best-effort — the stub agent's `sleep` lets the tmux
    // session die naturally.
    let tmux_name = format!("architect-{session_id}");
    kill_tmux_session(&tmux_bin, &root, &tmux_name);
    drop(pool);
    let _ = std::fs::remove_dir_all(&root);
}
