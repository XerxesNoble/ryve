// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Workshop schema migrations.
//!
//! Each workshop carries a `workshop_schema_version` in its
//! `.ryve/config.toml`. When a workshop is opened we compare it against
//! [`CURRENT_SCHEMA_VERSION`] (baked into the binary) and run any pending
//! migrations in order. Each migration is a named, idempotent function with
//! its own version number. After a migration succeeds the version is bumped
//! and the config is re-saved, so partial progress is durable.
//!
//! ## Adding a new migration
//!
//! 1. Bump [`CURRENT_SCHEMA_VERSION`] by one.
//! 2. Append a `(version, name)` entry to [`MIGRATIONS`].
//! 3. Add a matching arm in [`run_one`] dispatching to a new
//!    `migrate_vN_<name>` async function.
//! 4. Make the migration **idempotent** — re-running it on an already-migrated
//!    workshop must be a no-op.
//! 5. If the migration touches user-authored files, create a backup before
//!    overwriting (see invariants on the spark).
//!
//! Database schema migrations are explicitly out of scope — `sqlx::migrate!`
//! handles those when the pool is opened.

use crate::ryve_dir::{self, RyveDir, WorkshopConfig, save_config};

/// The schema version this binary knows how to produce.
///
/// Bump this whenever you add a new migration to [`MIGRATIONS`].
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// A migration that has been (or could be) applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRecord {
    pub version: u32,
    pub name: &'static str,
}

/// Result of running migrations against a workshop.
#[derive(Debug, Default, Clone)]
pub struct MigrationLog {
    /// Schema version stored in config before migrations ran.
    pub from_version: u32,
    /// Schema version after migrations ran (== `from_version` if nothing applied).
    pub to_version: u32,
    /// Migrations that were actually executed, in order.
    pub applied: Vec<MigrationRecord>,
}

impl MigrationLog {
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty()
    }

    /// Human-readable summary suitable for stdout or a UI toast.
    pub fn summary(&self) -> String {
        if self.applied.is_empty() {
            format!("Workshop schema up to date (v{})", self.to_version)
        } else {
            let mut s = format!(
                "Workshop migrated v{} → v{}:",
                self.from_version, self.to_version
            );
            for m in &self.applied {
                s.push_str(&format!("\n  • v{}: {}", m.version, m.name));
            }
            s
        }
    }
}

/// Ordered list of all known migrations.
///
/// Must be kept in ascending order by version. Each entry's version must be
/// `<= CURRENT_SCHEMA_VERSION`.
const MIGRATIONS: &[(u32, &str)] = &[(1, "ensure_base_layout")];

/// Dispatch table mapping a version number to its implementation.
async fn run_one(version: u32, ryve_dir: &RyveDir) -> std::io::Result<()> {
    match version {
        1 => migrate_v1_ensure_base_layout(ryve_dir).await,
        v => Err(std::io::Error::other(format!(
            "unknown workshop migration version {v}"
        ))),
    }
}

/// v1 — establish the standard `.ryve/` directory layout.
///
/// Creates the standard subdirectories, default config (if missing), default
/// `context/AGENTS.md`, and default `checklists/DONE.md`. This is the layout
/// that pre-migration `init_ryve_dir` produced, captured as an explicit,
/// versioned step. Idempotent: each file is only written if it does not
/// already exist.
async fn migrate_v1_ensure_base_layout(ryve_dir: &RyveDir) -> std::io::Result<()> {
    ryve_dir.ensure_exists().await?;

    if !ryve_dir.agents_md_path().exists() {
        tokio::fs::write(ryve_dir.agents_md_path(), ryve_dir::DEFAULT_AGENTS_MD).await?;
    }

    if !ryve_dir.done_md_path().exists() {
        tokio::fs::write(ryve_dir.done_md_path(), ryve_dir::DEFAULT_DONE_MD).await?;
    }

    Ok(())
}

/// Bring a workshop's `.ryve/` directory up to [`CURRENT_SCHEMA_VERSION`].
///
/// - Creates `.ryve/` if it doesn't exist.
/// - Loads `config.toml` (or starts from defaults if missing — version 0).
/// - Runs every migration whose version is `> stored && <= CURRENT_SCHEMA_VERSION`,
///   in order. After each migration the config's version field is bumped and
///   re-saved so partial progress is recorded on disk.
/// - Returns the (possibly updated) config and a [`MigrationLog`] describing
///   what ran. The caller is responsible for surfacing the log (stdout or UI).
///
/// If the stored version is **higher** than `CURRENT_SCHEMA_VERSION` (i.e. the
/// workshop was last touched by a newer binary) this function leaves the
/// workshop alone and returns an empty log — downgrade is a non-goal.
pub async fn migrate_workshop(
    ryve_dir: &RyveDir,
) -> std::io::Result<(WorkshopConfig, MigrationLog)> {
    // We need .ryve/ to exist so we can read/write config.toml.
    tokio::fs::create_dir_all(ryve_dir.root()).await?;

    let mut config = ryve_dir::load_config(ryve_dir).await;
    let from = config.workshop_schema_version;

    let mut log = MigrationLog {
        from_version: from,
        to_version: from,
        applied: Vec::new(),
    };

    if from > CURRENT_SCHEMA_VERSION {
        // Future workshop opened by an older binary — leave it alone.
        return Ok((config, log));
    }

    // Validate the migration table at runtime: versions must be ascending and
    // every version must be reachable. Cheap and catches developer mistakes
    // early.
    debug_assert!(
        MIGRATIONS.windows(2).all(|w| w[0].0 < w[1].0),
        "MIGRATIONS must be ordered ascending by version"
    );
    debug_assert!(
        MIGRATIONS.last().map(|(v, _)| *v).unwrap_or(0) == CURRENT_SCHEMA_VERSION,
        "the last MIGRATIONS entry must equal CURRENT_SCHEMA_VERSION"
    );

    for &(version, name) in MIGRATIONS {
        if version <= from {
            continue;
        }
        if version > CURRENT_SCHEMA_VERSION {
            break;
        }

        run_one(version, ryve_dir).await?;

        config.workshop_schema_version = version;
        save_config(ryve_dir, &config).await.map_err(|e| {
            std::io::Error::other(format!(
                "saving config after migration v{version} ({name}): {e}"
            ))
        })?;

        log.applied.push(MigrationRecord { version, name });
        log.to_version = version;
    }

    // Brand new workshops have no config.toml yet — make sure one is written
    // even if the migration table happened to be empty above the floor.
    if !ryve_dir.config_path().exists() {
        save_config(ryve_dir, &config).await?;
    }

    Ok((config, log))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Tiny RAII temp dir — avoids pulling in `tempfile` just for these tests.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let p =
                std::env::temp_dir().join(format!("ryve-migrations-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).expect("create temp dir");
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        TempDir::new()
    }

    #[tokio::test]
    async fn fresh_workshop_runs_all_migrations() {
        let dir = tempdir();
        let ryve_dir = RyveDir::new(dir.path());

        let (config, log) = migrate_workshop(&ryve_dir).await.expect("migrate");

        assert_eq!(log.from_version, 0);
        assert_eq!(log.to_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(log.applied.len() as u32, CURRENT_SCHEMA_VERSION);
        assert_eq!(config.workshop_schema_version, CURRENT_SCHEMA_VERSION);

        // v1 artifacts should exist on disk.
        assert!(ryve_dir.agents_md_path().exists());
        assert!(ryve_dir.done_md_path().exists());
        assert!(ryve_dir.config_path().exists());

        // Persisted config should carry the bumped version.
        let reloaded = ryve_dir::load_config(&ryve_dir).await;
        assert_eq!(reloaded.workshop_schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn second_run_is_a_noop() {
        let dir = tempdir();
        let ryve_dir = RyveDir::new(dir.path());

        let (_c1, _log1) = migrate_workshop(&ryve_dir).await.expect("first");
        let (_c2, log2) = migrate_workshop(&ryve_dir).await.expect("second");

        assert!(
            log2.is_empty(),
            "second migration run should apply nothing, got {:?}",
            log2.applied
        );
        assert_eq!(log2.from_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(log2.to_version, CURRENT_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn migration_preserves_user_authored_files() {
        let dir = tempdir();
        let ryve_dir = RyveDir::new(dir.path());

        // Pretend the user already has custom AGENTS.md / DONE.md.
        ryve_dir.ensure_exists().await.unwrap();
        tokio::fs::write(ryve_dir.agents_md_path(), "USER AGENTS")
            .await
            .unwrap();
        tokio::fs::write(ryve_dir.done_md_path(), "USER DONE")
            .await
            .unwrap();

        migrate_workshop(&ryve_dir).await.expect("migrate");

        let agents = tokio::fs::read_to_string(ryve_dir.agents_md_path())
            .await
            .unwrap();
        let done = tokio::fs::read_to_string(ryve_dir.done_md_path())
            .await
            .unwrap();
        assert_eq!(agents, "USER AGENTS", "must not overwrite user content");
        assert_eq!(done, "USER DONE", "must not overwrite user content");
    }

    #[tokio::test]
    async fn future_version_is_left_alone() {
        let dir = tempdir();
        let ryve_dir = RyveDir::new(dir.path());

        // Simulate a workshop touched by a newer binary.
        ryve_dir.ensure_exists().await.unwrap();
        let mut cfg = WorkshopConfig::default();
        cfg.workshop_schema_version = CURRENT_SCHEMA_VERSION + 99;
        save_config(&ryve_dir, &cfg).await.unwrap();

        let (config, log) = migrate_workshop(&ryve_dir).await.expect("migrate");

        assert!(log.applied.is_empty());
        assert_eq!(config.workshop_schema_version, CURRENT_SCHEMA_VERSION + 99);
        // Persisted version is unchanged.
        let reloaded = ryve_dir::load_config(&ryve_dir).await;
        assert_eq!(
            reloaded.workshop_schema_version,
            CURRENT_SCHEMA_VERSION + 99
        );
    }

    #[tokio::test]
    async fn version_bump_is_persisted_between_runs() {
        let dir = tempdir();
        let ryve_dir = RyveDir::new(dir.path());

        migrate_workshop(&ryve_dir).await.expect("first");

        let on_disk = tokio::fs::read_to_string(ryve_dir.config_path())
            .await
            .unwrap();
        assert!(
            on_disk.contains("workshop_schema_version"),
            "config.toml should contain the version field, got:\n{on_disk}"
        );
    }

    #[test]
    fn summary_renders_applied_migrations() {
        let log = MigrationLog {
            from_version: 0,
            to_version: 1,
            applied: vec![MigrationRecord {
                version: 1,
                name: "ensure_base_layout",
            }],
        };
        let s = log.summary();
        assert!(s.contains("v0 → v1"));
        assert!(s.contains("ensure_base_layout"));
    }

    #[test]
    fn summary_renders_noop() {
        let log = MigrationLog {
            from_version: 1,
            to_version: 1,
            applied: vec![],
        };
        assert!(log.summary().contains("up to date"));
    }
}
