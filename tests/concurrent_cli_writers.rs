//! Multi-process stress test for the workgraph write-discipline policy.
//!
//! Spark `sp-fbf2a519` — "Write discipline: prevent concurrent-writer
//! corruption of sparks.db under a Crew" — requires that >=8 simulated
//! Hands writing to the same `sparks.db` concurrently keep the database
//! consistent (no corruption, no lost writes). A Hand, in production, is
//! an independent OS process that invokes the `ryve` CLI binary. The
//! in-process fan-out tests in `data/tests/concurrency_stress.rs` cover
//! one-process-many-tasks contention; this test covers the real
//! failure mode from the 2026-04-08 incident: **many independent
//! processes racing on the same database file**.
//!
//! Mechanism:
//!
//! 1. Create a fresh temp workshop and run `ryve init` in it (as a
//!    subprocess, so we exercise exactly the code path a user would).
//! 2. Spawn N independent `ryve spark create` subprocesses in parallel,
//!    each pointed at the same workshop via `RYVE_WORKSHOP_ROOT`.
//! 3. Wait for all of them, assert every exit code is 0.
//! 4. Open the database directly and assert:
//!    - row count in `sparks` equals N (no lost writes);
//!    - `PRAGMA integrity_check` returns "ok" (no corruption);
//!    - the DB can be reopened cleanly after the storm.
//!
//! See `docs/WRITE_DISCIPLINE.md` for the policy this test enforces.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;

use sqlx::Row;

/// Unique temp directory per test so parallel `cargo test` runs don't
/// stomp on each other and so leftover state from a previous run of the
/// same test never leaks in.
fn unique_tempdir(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ryve-cli-stress-{tag}-{pid}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Path to the `ryve` binary under test. `cargo test` sets this env var
/// for every integration test in a package that defines a `[[bin]]`
/// named `ryve`, which our top-level `Cargo.toml` does.
fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Run `ryve init` inside `workshop_dir` as a subprocess. Panics if the
/// subprocess fails — if init itself is broken, the rest of the test
/// can't report anything meaningful.
fn ryve_init(workshop_dir: &PathBuf) {
    let output = Command::new(ryve_bin())
        .arg("init")
        .current_dir(workshop_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to spawn `ryve init`");
    assert!(
        output.status.success(),
        "ryve init failed: status={:?} stdout={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        workshop_dir.join(".ryve").join("sparks.db").exists(),
        "ryve init did not create .ryve/sparks.db"
    );
}

/// At least 8 simulated Hands writing to `sparks.db` concurrently, each
/// in its own OS process. This is the acceptance test for spark
/// `sp-fbf2a519`'s second criterion.
///
/// Rationale for N=12: the spark floor is 8; we run noticeably more to
/// make sure we're actually exercising contention rather than skating
/// past it. Twelve concurrent writers is representative of a Head
/// fanning out a mid-sized Crew.
#[test]
fn twelve_concurrent_cli_hands_keep_sparks_db_intact() {
    const N: usize = 12;

    let workshop = unique_tempdir("crew");
    ryve_init(&workshop);

    // Spawn N writer processes in parallel. We use std threads to run
    // the subprocess blocking calls concurrently; the actual
    // concurrency that matters is at the OS-process level, not at the
    // thread level.
    let mut handles = Vec::with_capacity(N);
    for i in 0..N {
        let ws = workshop.clone();
        let bin = ryve_bin();
        handles.push(thread::spawn(move || {
            let title = format!("crew-writer-{i}");
            let output = Command::new(bin)
                .args([
                    "spark",
                    "create",
                    "--type",
                    "task",
                    "--priority",
                    "2",
                    &title,
                ])
                // RYVE_WORKSHOP_ROOT is honored by `cli::run` (see
                // src/cli.rs), which lets us point every subprocess at
                // the same workshop without any of them having to cd
                // into it or walk the directory tree. This is exactly
                // what a Hand does in production: its own cwd is
                // wherever its worktree is, but RYVE_WORKSHOP_ROOT
                // points at the shared workshop root.
                .env("RYVE_WORKSHOP_ROOT", &ws)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("failed to spawn `ryve spark create`");
            (i, output)
        }));
    }

    // Join every writer. Collect failures rather than panicking on the
    // first one so we see the whole picture if the policy regresses.
    let mut failures: Vec<String> = Vec::new();
    for h in handles {
        let (i, output) = h.join().expect("writer thread panicked");
        if !output.status.success() {
            failures.push(format!(
                "writer {i} failed: status={:?} stdout={:?} stderr={:?}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "one or more concurrent CLI writers failed — write-discipline \
         policy has regressed. See docs/WRITE_DISCIPLINE.md.\n{}",
        failures.join("\n")
    );

    // Now verify the policy invariant: the DB is consistent, every
    // write is durable, and `PRAGMA integrity_check` is happy. We
    // open the DB via the same `data::db` path the CLI uses — there
    // is deliberately only one way to open `sparks.db` in the repo,
    // so using it here keeps this test aligned with reality.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let pool = data::db::open_sparks_db(&workshop)
            .await
            .expect("reopen sparks.db after storm");

        // (a) no lost writes
        let row = sqlx::query("SELECT COUNT(*) FROM sparks")
            .fetch_one(&pool)
            .await
            .expect("count query");
        let count: i64 = row.get(0);
        assert_eq!(
            count, N as i64,
            "expected {N} rows after {N} concurrent CLI writers, got {count} — \
             lost writes indicate a broken busy_timeout / retry policy"
        );

        // (b) no corruption
        let row = sqlx::query("PRAGMA integrity_check")
            .fetch_one(&pool)
            .await
            .expect("integrity_check query");
        let result: String = row.get(0);
        assert_eq!(
            result.to_lowercase(),
            "ok",
            "PRAGMA integrity_check != ok after concurrent CLI writers \
             (got {result:?}) — sparks.db is corrupt"
        );

        // (c) titles all present — verifies that not just the row count
        // but the *contents* survived contention. This would catch a
        // hypothetical bug where N rows exist but some writes were
        // clobbered by a later writer on the same id.
        let rows = sqlx::query("SELECT title FROM sparks ORDER BY title")
            .fetch_all(&pool)
            .await
            .expect("title query");
        let mut titles: Vec<String> = rows.into_iter().map(|r| r.get::<String, _>(0)).collect();
        titles.sort();
        let mut expected: Vec<String> = (0..N).map(|i| format!("crew-writer-{i}")).collect();
        expected.sort();
        assert_eq!(
            titles, expected,
            "row contents diverged from expected set of writer titles — \
             writes were clobbered under contention"
        );

        pool.close().await;

        // (d) reopenable — the strongest end-to-end evidence that the
        // WAL + main file pair is still internally consistent.
        let reopened = data::db::open_sparks_db(&workshop)
            .await
            .expect("second reopen after storm");
        let row = sqlx::query("SELECT COUNT(*) FROM sparks")
            .fetch_one(&reopened)
            .await
            .unwrap();
        let count: i64 = row.get(0);
        assert_eq!(count, N as i64);
        reopened.close().await;
    });

    let _ = std::fs::remove_dir_all(&workshop);
}
