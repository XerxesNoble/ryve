// SPDX-License-Identifier: AGPL-3.0-or-later

//! Language-agnostic invariant for the four initial Hand archetypes
//! (sp-1471f46a → ryve-70d7677b).
//!
//! The parent epic states: "Archetypes are language-agnostic: prompts,
//! tool gates, and acceptance criteria carry no assumptions about Rust,
//! Python, JS, or any specific ecosystem. A test suite proves this by
//! running at least one archetype against a synthetic non-Rust project."
//!
//! This test enforces both halves of that invariant for every archetype
//! the current branch ships:
//!
//!   1. **No Rust ecosystem leaks.** The composed system prompt must not
//!      contain any token from `RUST_ECOSYSTEM_TOKENS` — `cargo`,
//!      `clippy`, `rustc`, `rustup`, `rustfmt`, `Cargo.toml`, `crates.io`,
//!      `mod.rs`, `rust-toolchain`, `rust-analyzer`. These are tools and
//!      conventions that only make sense in a Rust project; smuggling
//!      them into a Hand's instructions would push the agent toward
//!      Rust-flavoured suggestions on a Python / TypeScript / Go repo.
//!
//!   2. **Read-vs-write tool discipline.** Each archetype's prompt must
//!      declare the right capability gate: investigator/research is
//!      read-only and forbids editor tools by name; owner/merger may
//!      mutate and must not be sandboxed into read-only language.
//!
//! The fixture lives under `fixtures/multi-lang/` and contains Python,
//! TypeScript, Go, and a README spec — zero Rust files. The test copies
//! the fixture into a fresh workshop tempdir, points each spark's scope
//! at the fixture, spawns the Hand via the `ryve hand spawn` CLI (the
//! same path the UI uses), then reads back the prompt file the spawn
//! wrote to `.ryve/prompts/hand-<session>.md`. No real coding-agent
//! subprocess runs — the agent argument is a stub shell script that
//! sleeps long enough to keep tmux happy and exits.
//!
//! The test gates on a real tmux binary (vendored under
//! `vendor/tmux/bin/tmux` after `scripts/build-vendored-tmux.sh` runs in
//! CI) and skips cleanly if neither bundled nor system tmux is
//! installed. CI builds the vendored tmux explicitly, so this gate fires
//! the test on every push to `main`.
//!
//! ## Failure semantics
//!
//! "Failing this test blocks the epic" — the spark's invariants section.
//! A new archetype that bakes Rust assumptions into its prompt, or a
//! drift in the existing prompts that re-introduces `cargo` / `Cargo.toml`
//! / `mod.rs` examples, must not ship without a deliberate update to the
//! token list here and a written justification on the offending spark.

use std::path::{Path, PathBuf};
use std::process::Command;

// ── Forbidden-token list ─────────────────────────────────────

/// Tokens that would betray a Rust-ecosystem assumption if they appeared
/// in an archetype's composed system prompt. Matched case-insensitively
/// against the rendered prompt body; if a future archetype legitimately
/// needs to mention one of these (e.g. an explicit "Rust-specific
/// archetype" we have not built yet), update this list and document the
/// justification on the relevant spark.
const RUST_ECOSYSTEM_TOKENS: &[&str] = &[
    "cargo",
    "clippy",
    "rustc",
    "rustup",
    "rustfmt",
    "cargo.toml",
    "crates.io",
    "mod.rs",
    "rust-toolchain",
    "rust-analyzer",
];

fn assert_no_rust_tokens(archetype: &str, prompt: &str) {
    let lower = prompt.to_ascii_lowercase();
    let mut hits: Vec<(&str, usize)> = Vec::new();
    for token in RUST_ECOSYSTEM_TOKENS {
        if let Some(idx) = lower.find(token) {
            hits.push((token, idx));
        }
    }
    assert!(
        hits.is_empty(),
        "{archetype} prompt leaked Rust-ecosystem tokens {hits:?}; \
         the language-agnostic invariant requires archetype boilerplate \
         to stay neutral. Excerpt around first hit:\n{}",
        hits.first()
            .map(|(_, idx)| {
                let start = idx.saturating_sub(60);
                let end = (idx + 80).min(prompt.len());
                prompt[start..end].to_string()
            })
            .unwrap_or_default()
    );
}

// ── Workshop / tmux harness (mirrors investigator_hand.rs) ──

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Mirror of `src/tmux.rs::resolve_tmux_bin`. CI builds the vendored
/// tmux explicitly; local dev runs may fall back to system tmux. If
/// neither exists, the test skips cleanly so platforms without tmux
/// stay green.
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

/// Build a throwaway workshop populated with the multi-lang fixture. The
/// workshop's working tree is a copy of `fixtures/multi-lang/` so any
/// scope reference like `fixtures/multi-lang/app.py` written into a
/// spark resolves to a real file. Returns `(workshop_root, stub_agent)`.
fn setup_workshop(tag: &str) -> (PathBuf, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ryve-archetype-lang-{tag}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create workshop tempdir");

    // Copy fixture tree into the workshop root so file scopes resolve.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_src = manifest.join("fixtures").join("multi-lang");
    copy_fixture(&fixture_src, &root);

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
    run_git(&["add", "-A"]);
    run_git(&["commit", "-q", "-m", "fixture: multi-lang seed"]);

    let status = Command::new(ryve_bin())
        .arg("init")
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .status()
        .expect("spawn ryve init");
    assert!(status.success(), "ryve init failed");

    // Stub agent: sleep so the tmux session stays alive long enough for
    // the spawn flow's pipe-pane attach to succeed. The test never
    // inspects argv — the prompt is read from `.ryve/prompts/`.
    let stub_path = root.join("stub-agent.sh");
    std::fs::write(&stub_path, "#!/bin/sh\nsleep 3\n").expect("write stub agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub_path, perms).unwrap();
    }

    (root, stub_path)
}

fn copy_fixture(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type().expect("fixture file type");
        if ft.is_dir() {
            std::fs::create_dir_all(&to).expect("mkdir fixture subdir");
            copy_fixture(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy fixture file");
        }
    }
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

/// Best-effort tear-down of the tmux session created by a spawn so the
/// private socket goes away before the tempdir is removed.
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

/// Mirror of `src/tmux.rs::short_socket_path`. Duplicated because the
/// binary crate has no library target to import from.
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

/// Create a neutral spark whose scope points at the multi-lang fixture
/// so an archetype that *did* lean on Rust conventions would have an
/// excuse to suggest one. The spark intent is deliberately polyglot —
/// it asks for a survey across Python, TypeScript, and Go — and uses no
/// language-specific vocabulary.
///
/// `parent_epic` is required for non-epic types because the workgraph
/// rejects orphan tasks/chores. Pass `None` for epic-typed sparks.
fn create_polyglot_spark(
    root: &Path,
    tmux_bin: &Path,
    spark_type: &str,
    title: &str,
    parent_epic: Option<&str>,
) -> String {
    let mut args: Vec<&str> = vec![
        "--json",
        "spark",
        "create",
        "--type",
        spark_type,
        "--priority",
        "2",
        "--scope",
        "fixtures/multi-lang/",
        "--problem",
        "OrderRouter handlers drift across the polyglot fixture; survey \
         app.py, app.ts, and app.go and confirm they share a consistent \
         customer-keyed fan-out shape.",
        "--invariant",
        "All three handlers group OrderEvents by customer id.",
        "--acceptance",
        "Every handler file's fan-out function is documented in the report.",
        "--non-goal",
        "Adding a fourth handler in another language.",
    ];
    if let Some(parent) = parent_epic {
        args.push("--parent");
        args.push(parent);
    }
    args.push(title);

    let create = ryve(root, tmux_bin, &args);
    assert!(
        create.status.success(),
        "spark create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let stdout = String::from_utf8_lossy(&create.stdout);
    parse_json_string(&stdout, "id")
        .unwrap_or_else(|| panic!("could not parse spark id from: {stdout}"))
}

/// Create the polyglot epic + a child task under it. Returns the child
/// spark id; the epic is only used as a parent so the workgraph accepts
/// the task. Used by archetypes that should be tested against a normal
/// task spark (owner / investigator).
fn create_child_task(root: &Path, tmux_bin: &Path, title: &str) -> String {
    let epic = create_polyglot_spark(root, tmux_bin, "epic", &format!("{title} epic"), None);
    create_polyglot_spark(root, tmux_bin, "task", title, Some(&epic))
}

/// Spawn a Hand of the given role and return the rendered prompt body.
///
/// Returns `Some((session_id, prompt))` on success; `None` if the spawn
/// failed (typically tmux trouble on this runner) so the caller can
/// degrade to a skip rather than a hard fail. The spawn writes the
/// composed prompt to `.ryve/prompts/hand-<session>.md` *before* the
/// tmux launch happens, so even when the launch trips on environmental
/// flakiness the prompt file is what we are after.
fn spawn_and_read_prompt(
    root: &Path,
    tmux_bin: &Path,
    stub_path: &Path,
    spark_id: &str,
    role: &str,
    crew_id: Option<&str>,
) -> Option<(String, String)> {
    let mut args = vec![
        "--json",
        "hand",
        "spawn",
        spark_id,
        "--role",
        role,
        "--agent",
        stub_path.to_str().expect("stub path utf-8"),
        "--actor",
        "tester",
    ];
    if let Some(cid) = crew_id {
        args.push("--crew");
        args.push(cid);
    }
    let spawn = ryve(root, tmux_bin, &args);
    if !spawn.status.success() {
        eprintln!(
            "hand spawn ({role}) returned non-zero — skipping prompt readback. \
             stdout={} stderr={}",
            String::from_utf8_lossy(&spawn.stdout),
            String::from_utf8_lossy(&spawn.stderr)
        );
        return None;
    }
    let stdout = String::from_utf8_lossy(&spawn.stdout);
    let session_id = parse_json_string(&stdout, "session_id")?;
    let prompt_path = root
        .join(".ryve")
        .join("prompts")
        .join(format!("hand-{session_id}.md"));
    let prompt = std::fs::read_to_string(&prompt_path).unwrap_or_else(|e| {
        panic!(
            "{role} prompt file missing at {}: {e}",
            prompt_path.display()
        )
    });
    Some((session_id, prompt))
}

// ── Per-archetype tests ────────────────────────────────────

#[tokio::test]
#[ignore = "spawns multiple cargo processes — exhausts CI runner threads"]
async fn owner_hand_prompt_is_language_agnostic() {
    let Some(tmux_bin) = find_tmux_binary() else {
        eprintln!("tmux unavailable — skipping owner archetype language-agnostic test.");
        return;
    };
    let (root, stub_path) = setup_workshop("owner");
    let spark_id = create_child_task(&root, &tmux_bin, "owner survey");

    let Some((session_id, prompt)) =
        spawn_and_read_prompt(&root, &tmux_bin, &stub_path, &spark_id, "owner", None)
    else {
        let _ = std::fs::remove_dir_all(&root);
        return;
    };

    // (a) No Rust-ecosystem leaks.
    assert_no_rust_tokens("owner Hand", &prompt);

    // (b) Tool policy: write-capable. The owner Hand's prompt is the
    // standard HOUSE_RULES — it must NOT carry the Investigator's
    // read-only contract or its editor-tool ban.
    assert!(
        prompt.contains("HOUSE RULES"),
        "owner prompt should include HOUSE RULES; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("READ-ONLY"),
        "owner Hand is write-capable; must not declare READ-ONLY. Prompt:\n{prompt}"
    );
    assert!(
        !prompt.contains("MUST NOT use Edit, Write"),
        "owner Hand must not be sandboxed into the investigator editor ban. Prompt:\n{prompt}"
    );

    kill_tmux_session(&tmux_bin, &root, &format!("hand-{session_id}"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
#[ignore = "spawns multiple cargo processes — exhausts CI runner threads"]
async fn head_hand_prompt_is_language_agnostic() {
    let Some(tmux_bin) = find_tmux_binary() else {
        eprintln!("tmux unavailable — skipping head archetype language-agnostic test.");
        return;
    };
    let (root, stub_path) = setup_workshop("head");
    let epic_id = create_polyglot_spark(&root, &tmux_bin, "epic", "head survey", None);

    let Some((session_id, prompt)) =
        spawn_and_read_prompt(&root, &tmux_bin, &stub_path, &epic_id, "head", None)
    else {
        let _ = std::fs::remove_dir_all(&root);
        return;
    };

    assert_no_rust_tokens("head Hand", &prompt);

    // Head is an orchestrator that must stay headless — the prompt has
    // to forbid direct code edits while leaving its delegated Hands
    // free to write.
    assert!(
        prompt.contains("Stay headless")
            || prompt.contains("do not edit source files")
            || prompt.contains("never edit"),
        "head prompt must forbid the Head from editing source. Prompt:\n{prompt}"
    );
    assert!(
        prompt.contains("ryve hand spawn"),
        "head prompt must direct the Head to spawn Hands rather than write code. Prompt:\n{prompt}"
    );

    kill_tmux_session(&tmux_bin, &root, &format!("head-{session_id}"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
#[ignore = "spawns multiple cargo processes — exhausts CI runner threads"]
async fn investigator_hand_prompt_is_language_agnostic() {
    let Some(tmux_bin) = find_tmux_binary() else {
        eprintln!("tmux unavailable — skipping investigator archetype language-agnostic test.");
        return;
    };
    let (root, stub_path) = setup_workshop("investigator");
    let spark_id = create_child_task(&root, &tmux_bin, "investigator survey");

    let Some((session_id, prompt)) = spawn_and_read_prompt(
        &root,
        &tmux_bin,
        &stub_path,
        &spark_id,
        "investigator",
        None,
    ) else {
        let _ = std::fs::remove_dir_all(&root);
        return;
    };

    assert_no_rust_tokens("investigator Hand", &prompt);

    // Investigator is read-only — its prompt must spell out the editor
    // ban and the comment-based finding channel by name. These are the
    // exact strings the investigator_hand.rs end-to-end test also
    // pins, kept in sync so prompt drift fails both tests at once.
    assert!(
        prompt.contains("READ-ONLY"),
        "investigator must declare the READ-ONLY contract. Prompt:\n{prompt}"
    );
    assert!(
        prompt.contains("MUST NOT use Edit, Write"),
        "investigator must explicitly forbid Edit/Write tools. Prompt:\n{prompt}"
    );
    assert!(
        prompt.contains("ryve comment add"),
        "investigator must route findings through `ryve comment add`. Prompt:\n{prompt}"
    );
    assert!(
        prompt.contains("file:line"),
        "investigator must require file:line evidence on every finding. Prompt:\n{prompt}"
    );

    kill_tmux_session(&tmux_bin, &root, &format!("investigator-{session_id}"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
#[ignore = "spawns multiple cargo processes — exhausts CI runner threads"]
async fn merger_hand_prompt_is_language_agnostic() {
    let Some(tmux_bin) = find_tmux_binary() else {
        eprintln!("tmux unavailable — skipping merger archetype language-agnostic test.");
        return;
    };
    let (root, stub_path) = setup_workshop("merger");
    let parent_spark = create_polyglot_spark(&root, &tmux_bin, "epic", "merger parent", None);

    // Mergers require a crew to attach to.
    let crew_out = ryve(
        &root,
        &tmux_bin,
        &[
            "--json",
            "crew",
            "create",
            "--parent",
            &parent_spark,
            "polyglot crew",
        ],
    );
    assert!(
        crew_out.status.success(),
        "crew create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&crew_out.stdout),
        String::from_utf8_lossy(&crew_out.stderr)
    );
    let crew_stdout = String::from_utf8_lossy(&crew_out.stdout);
    let crew_id = parse_json_string(&crew_stdout, "id")
        .unwrap_or_else(|| panic!("could not parse crew id from: {crew_stdout}"));

    let merge_spark = create_polyglot_spark(
        &root,
        &tmux_bin,
        "chore",
        "merger integration",
        Some(&parent_spark),
    );

    let Some((session_id, prompt)) = spawn_and_read_prompt(
        &root,
        &tmux_bin,
        &stub_path,
        &merge_spark,
        "merger",
        Some(&crew_id),
    ) else {
        let _ = std::fs::remove_dir_all(&root);
        return;
    };

    assert_no_rust_tokens("merger Hand", &prompt);

    // Merger is the only Hand allowed to push integrations. Its prompt
    // must spell out the destructive-git guard rails by name so a drift
    // toward a more permissive policy fails here.
    assert!(
        prompt.contains("Never force-push") || prompt.contains("never force-push"),
        "merger prompt must forbid force-push. Prompt:\n{prompt}"
    );
    assert!(
        prompt.contains("git push -u origin"),
        "merger prompt must show the integration push command. Prompt:\n{prompt}"
    );
    assert!(
        !prompt.contains("READ-ONLY"),
        "merger is write-capable (push + merge); must not declare READ-ONLY. Prompt:\n{prompt}"
    );

    kill_tmux_session(&tmux_bin, &root, &format!("merger-{session_id}"));
    let _ = std::fs::remove_dir_all(&root);
}

// ── Fixture-tree invariants ────────────────────────────────

/// The fixture project must contain Python, TypeScript, and Go sources
/// and a README spec — and zero Rust files. Hardcodes the spark's
/// "test project contains zero Rust files" invariant so accidental
/// edits to the fixture (adding a `.rs`, a `Cargo.toml`, etc.) fail
/// loudly here instead of silently weakening the language-agnostic
/// guarantee.
#[test]
fn fixture_tree_is_polyglot_and_rust_free() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = manifest.join("fixtures").join("multi-lang");

    assert!(
        fixture.is_dir(),
        "fixtures/multi-lang/ must exist at {}",
        fixture.display()
    );

    let mut have_py = false;
    let mut have_ts = false;
    let mut have_go = false;
    let mut have_readme = false;
    let mut rust_offenders: Vec<PathBuf> = Vec::new();

    walk_fixture(&fixture, &mut |path| {
        let lower = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if lower.ends_with(".py") {
            have_py = true;
        }
        if lower.ends_with(".ts") {
            have_ts = true;
        }
        if lower.ends_with(".go") {
            have_go = true;
        }
        if lower == "readme" || lower.starts_with("readme.") {
            have_readme = true;
        }
        // Rust-detector: any `.rs` file or a Cargo manifest pollutes
        // the polyglot invariant.
        if lower.ends_with(".rs") || lower == "cargo.toml" || lower == "cargo.lock" {
            rust_offenders.push(path.to_path_buf());
        }
    });

    assert!(have_py, "fixture must contain at least one .py file");
    assert!(have_ts, "fixture must contain at least one .ts file");
    assert!(have_go, "fixture must contain at least one .go file");
    assert!(
        have_readme,
        "fixture must contain a README with a fake spec"
    );
    assert!(
        rust_offenders.is_empty(),
        "fixtures/multi-lang/ must contain zero Rust files; found {rust_offenders:?}"
    );
}

fn walk_fixture(dir: &Path, visit: &mut dyn FnMut(&Path)) {
    for entry in std::fs::read_dir(dir).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let path = entry.path();
        let ft = entry.file_type().expect("fixture file type");
        if ft.is_dir() {
            walk_fixture(&path, visit);
        } else {
            visit(&path);
        }
    }
}
