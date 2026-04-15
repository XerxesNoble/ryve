//! Repo hygiene guard: SQLite sparks.db and its sidecars must never be
//! tracked in git.
//!
//! Why this matters:
//! SQLite treats `sparks.db`, `sparks.db-wal`, and `sparks.db-shm` as a
//! single atomic unit. If any one of them is versioned (even just the
//! sidecars), a `git stash` or `git checkout` on a live workshop rips the
//! sidecars out from under the running Ryve process. The writers keep
//! appending to the main database, atomicity is lost, and the workgraph
//! corrupts beyond `.recover` salvage — exactly the root cause of the
//! 2026-04-08 data loss incident (spark sp-b862594d).
//!
//! This test runs `git ls-files` from the repository root and fails if any
//! path matching `sparks.db` or a SQLite sidecar extension is present in the
//! index.

use std::process::Command;

fn repo_root() -> std::path::PathBuf {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .expect("git rev-parse --show-toplevel");
    assert!(output.status.success(), "git rev-parse failed");
    let path = String::from_utf8(output.stdout).expect("utf8");
    std::path::PathBuf::from(path.trim())
}

fn is_sparks_db_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    // Matches the SQLite atomic unit: sparks.db plus any `-suffix` sidecar
    // (-wal, -shm, -journal, -journal123, etc.). Deliberately does NOT match
    // unrelated files like `sparks.db.md` docs, which use a `.` separator.
    if name == "sparks.db" {
        return true;
    }
    if let Some(rest) = name.strip_prefix("sparks.db-") {
        // Require at least one trailing character so "sparks.db-" alone
        // doesn't slip through as a false positive.
        return !rest.is_empty();
    }
    false
}

#[test]
fn sparks_db_and_sidecars_are_not_tracked() {
    let root = repo_root();
    let output = Command::new("git")
        .arg("ls-files")
        .current_dir(&root)
        .output()
        .expect("git ls-files");
    assert!(output.status.success(), "git ls-files failed");
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    let offenders: Vec<&str> = stdout
        .lines()
        .filter(|line| is_sparks_db_path(line))
        .collect();

    assert!(
        offenders.is_empty(),
        "sparks.db file(s) are tracked in git — this corrupts the workgraph \
         on stash/checkout. Offending paths: {offenders:?}. \
         Run `git rm --cached <path>` and ensure .gitignore covers \
         `.ryve/sparks.db` and `.ryve/sparks.db-*`. \
         See docs/WORKGRAPH.md."
    );
}

fn is_backup_db_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "backups" && i > 0 && parts[i - 1] == ".ryve" {
            if let Some(filename) = parts.get(i + 1) {
                return filename.ends_with(".db");
            }
        }
    }
    false
}

#[test]
fn backup_db_files_are_not_tracked() {
    let root = repo_root();
    let output = Command::new("git")
        .arg("ls-files")
        .current_dir(&root)
        .output()
        .expect("git ls-files");
    assert!(output.status.success(), "git ls-files failed");
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    let offenders: Vec<&str> = stdout
        .lines()
        .filter(|line| is_backup_db_path(line))
        .collect();

    assert!(
        offenders.is_empty(),
        "Backup .db file(s) are tracked in git — .ryve/backups/*.db must be \
         ignored. Offending paths: {offenders:?}. \
         Run `git rm --cached <path>` and ensure .gitignore covers \
         `.ryve/backups/`. See docs/WORKGRAPH.md."
    );
}

#[test]
fn backup_path_matcher_recognizes_variants() {
    assert!(is_backup_db_path(".ryve/backups/sparks-20260414.db"));
    assert!(is_backup_db_path(".ryve/backups/sparks-1713052800.db"));
    assert!(is_backup_db_path(".ryve/backups/other.db"));

    assert!(!is_backup_db_path(".ryve/sparks.db"));
    assert!(!is_backup_db_path(".ryve/backups/readme.md"));
    assert!(!is_backup_db_path("backups/sparks-20260414.db"));
    assert!(!is_backup_db_path("other/backups/sparks.db"));
}

#[test]
fn path_matcher_recognizes_sidecar_variants() {
    assert!(is_sparks_db_path("sparks.db"));
    assert!(is_sparks_db_path(".ryve/sparks.db"));
    assert!(is_sparks_db_path(".ryve/sparks.db-wal"));
    assert!(is_sparks_db_path(".ryve/sparks.db-shm"));
    assert!(is_sparks_db_path(".ryve/sparks.db-journal"));
    assert!(is_sparks_db_path(".ryve/sparks.db-journal123"));
    assert!(is_sparks_db_path("some/nested/sparks.db-wal"));

    assert!(!is_sparks_db_path("sparks.rs"));
    assert!(!is_sparks_db_path("other.db"));
    assert!(!is_sparks_db_path("sparks.dbx"));
    assert!(!is_sparks_db_path("sparksXdb"));
    assert!(!is_sparks_db_path("docs/sparks.db.md"));
    assert!(!is_sparks_db_path(".ryve/sparks.db-"));
}
