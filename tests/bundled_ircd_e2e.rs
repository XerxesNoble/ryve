// SPDX-License-Identifier: AGPL-3.0-or-later

//! End-to-end: fresh `ryve init` → bundled ngIRCd on loopback → Ryve IPC
//! runtime dials it → an independent IRC client joins the channel →
//! an outbox event traverses the relay and arrives at the client.
//!
//! Locks in the 0.2.0 "IRC works out of the box" acceptance
//! (epic ryve-31659bbb). Every byte stays on 127.0.0.1:<bundled_port>,
//! so the test passes in CI without outbound network access.
//!
//! Skipped cleanly (not a failure) when the vendored ngIRCd binary is
//! absent — mirrors `tests/tmux_integration.rs`. CI with build deps
//! (cc, make) produces the binary via `build.rs`; minimal CI images
//! that only run `cargo check` / `cargo clippy` skip the build and
//! therefore skip this test.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use data::db::open_sparks_db;
use data::ryve_dir::{RyveDir, load_config};
use ipc::channel_manager::{EpicRef, channel_name};
use ipc::irc_client::{ConnectConfig, IrcClient, IrcMessage, MessageCallback};
use ipc::lifecycle::{IrcLifecycleConfig, IrcRuntime, LoggingExecutor};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::Mutex;

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

fn ngircd_bin() -> PathBuf {
    PathBuf::from(env!("RYVE_IRCD_DEV_PATH"))
}

async fn wait_for_port(port: u16, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn spawn_bundled_ngircd(workshop_root: &Path) -> Child {
    let conf = workshop_root.join(".ryve/ircd/ircd.conf");
    assert!(
        conf.exists(),
        "ryve init must write {} before the daemon starts",
        conf.display()
    );
    let mut cmd = TokioCommand::new(ngircd_bin());
    cmd.arg("--config")
        .arg(&conf)
        .arg("--nodaemon")
        // kill_on_drop ensures the daemon is reaped even when the test
        // panics before the explicit shutdown at the bottom.
        .kill_on_drop(true);
    cmd.spawn()
        .unwrap_or_else(|e| panic!("failed to spawn bundled ngircd: {e}"))
}

/// Poll the message log for a PRIVMSG on `channel` coming from a sender
/// other than the observer's own nick. Without the self-filter the test
/// would race against the observer's own JOIN echo before the relay's
/// PRIVMSG has a chance to land.
async fn wait_for_privmsg_on(
    received: &Arc<Mutex<Vec<IrcMessage>>>,
    channel: &str,
    own_nick: &str,
    budget: Duration,
) -> Option<IrcMessage> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        {
            let guard = received.lock().await;
            let hit = guard.iter().find(|m| {
                m.command == "PRIVMSG"
                    && m.params.first().map(String::as_str) == Some(channel)
                    && m.prefix
                        .as_deref()
                        .and_then(|p| p.split('!').next())
                        .map(|n| n != own_nick)
                        .unwrap_or(true)
            });
            if let Some(msg) = hit {
                return Some(msg.clone());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn seed_open_epic(pool: &SqlitePool, workshop_id: &str, id: &str, title: &str) {
    sqlx::query(
        "INSERT INTO sparks \
           (id, title, description, status, priority, spark_type, workshop_id, \
            created_at, updated_at) \
         VALUES (?, ?, '', 'open', 1, 'epic', ?, \
                 '2026-04-19T09:00:00Z', '2026-04-19T09:00:00Z')",
    )
    .bind(id)
    .bind(title)
    .bind(workshop_id)
    .execute(pool)
    .await
    .expect("seed epic");
}

async fn enqueue_assignment_created(
    pool: &SqlitePool,
    event_id: &str,
    epic_id: &str,
    epic_name: &str,
) {
    let payload = serde_json::json!({
        "epic_id": epic_id,
        "epic_name": epic_name,
        "assignment_id": "asg-e2e",
        "actor": "alice",
    });
    sqlx::query(
        "INSERT INTO event_outbox \
           (event_id, schema_version, timestamp, assignment_id, actor_id, \
            event_type, payload) \
         VALUES (?, 1, '2026-04-19T10:00:00Z', 'asg-e2e', 'actor-alice', \
                 'assignment.created', ?)",
    )
    .bind(event_id)
    .bind(payload.to_string())
    .execute(pool)
    .await
    .expect("insert outbox row");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_init_connects_ipc_runtime_and_delivers_event_on_loopback() {
    let ircd = ngircd_bin();
    if !ircd.exists() {
        eprintln!(
            "vendored ngIRCd binary not present at {} — skipping (build prerequisites likely \
             missing; see docs/VENDORED_IRCD.md)",
            ircd.display()
        );
        return;
    }

    // ── Step 1: `ryve init` in a TempDir ─────────────────────
    let workshop = TempDir::new().expect("tempdir");
    let status = Command::new(ryve_bin())
        .current_dir(workshop.path())
        .arg("init")
        .status()
        .expect("spawn ryve init");
    assert!(status.success(), "`ryve init` exited with {status}");

    let ryve_dir = RyveDir::new(workshop.path());
    let config = load_config(&ryve_dir).await;
    assert!(
        config.irc_enabled(),
        "fresh `ryve init` must leave IRC enabled"
    );
    let bundled_port = config
        .irc_bundled_port
        .expect("`ryve init` must record irc_bundled_port");

    // ── Step 2: bring up the workshop-scoped bundled daemon ─
    // Holds the Child so kill_on_drop(true) reaps ngircd even if the
    // test panics. The explicit shutdown at the bottom does the graceful
    // path; this is just the safety net.
    let mut daemon = spawn_bundled_ngircd(workshop.path());
    assert!(
        wait_for_port(bundled_port, Duration::from_secs(15)).await,
        "bundled ngIRCd did not bind 127.0.0.1:{bundled_port}"
    );

    // ── Step 3: open the DB and seed a single open epic ─────
    // The runtime joins a channel per open epic on boot; a single seeded
    // epic is enough to prove the join + relay path without depending on
    // workshop-id generation in `ryve init`.
    let workshop_id = "ws-e2e";
    let epic_id = "e2e";
    let epic_title = "E2E";
    // `ryve init` already ran migrations; reopening through the shared
    // helper is idempotent and gives us the WAL / busy_timeout /
    // foreign_keys configuration the relay and repo queries expect.
    let pool = open_sparks_db(workshop.path())
        .await
        .expect("open sparks.db created by ryve init");
    seed_open_epic(&pool, workshop_id, epic_id, epic_title).await;

    let channel = channel_name(&EpicRef {
        id: epic_id.into(),
        name: epic_title.into(),
    });

    // ── Step 4: the independent IRC client joins the channel ─
    let received: Arc<Mutex<Vec<IrcMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cb = Arc::clone(&received);
    let callback: MessageCallback = Arc::new(move |msg: IrcMessage| {
        let received_cb = Arc::clone(&received_cb);
        tokio::spawn(async move {
            received_cb.lock().await.push(msg);
        });
    });
    // RFC 2812 caps nicks at 9 chars; ngIRCd enforces that and closes the
    // registration with `432 Nickname too long` if we exceed it. The nick
    // must also differ from the runtime's (which defaults to "ryve").
    let observer_nick = "rvobs";
    let observer = IrcClient::connect(
        ConnectConfig::new("127.0.0.1", bundled_port, false, observer_nick, None),
        callback,
    )
    .await
    .expect("observer IRC client connected to bundled daemon");
    observer
        .join(&channel)
        .await
        .expect("observer joins epic channel");

    // ── Step 5: start the Ryve IPC runtime against the bundled daemon ─
    let lifecycle_cfg = IrcLifecycleConfig::from_workshop(&config, workshop_id)
        .expect("from_workshop resolves to the bundled loopback address");
    assert_eq!(
        lifecycle_cfg.server, "127.0.0.1",
        "bundled config must dial loopback, not an external server"
    );
    assert_eq!(lifecycle_cfg.port, bundled_port);
    assert_ne!(
        lifecycle_cfg.nick, observer_nick,
        "runtime nick must not collide with the observer"
    );

    let runtime = IrcRuntime::start(pool.clone(), lifecycle_cfg, Arc::new(LoggingExecutor))
        .await
        .expect("Ryve IPC runtime connects to the bundled daemon");

    // ── Step 6: enqueue an allow-listed event and wait for delivery ─
    enqueue_assignment_created(&pool, "evt-e2e", epic_id, epic_title).await;

    let delivered =
        wait_for_privmsg_on(&received, &channel, observer_nick, Duration::from_secs(10))
            .await
            .expect("observer client received a PRIVMSG on the epic channel");
    let body = delivered.params.get(1).cloned().unwrap_or_default();
    assert!(
        body.to_lowercase().contains("alice"),
        "rendered PRIVMSG should mention the actor; got body={body:?}"
    );

    // ── Clean shutdown ───────────────────────────────────────
    runtime.shutdown().await;
    let _ = observer.disconnect().await;
    pool.close().await;
    let _ = daemon.start_kill();
    let _ = daemon.wait().await;
}
