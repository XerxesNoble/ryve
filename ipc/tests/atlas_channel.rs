// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for the `#atlas` well-known workshop channel
//! (spark `ryve-850c0242`).
//!
//! Acceptance:
//! - `#atlas` exists on workshop open, joined by the Atlas actor by
//!   default, and is preserved across launches.
//! - On Atlas boot, a post `atlas <instance-id> online, seat
//!   <claim|follower>` appears in `irc_messages` for `#atlas`.
//! - On graceful shutdown, a seat-released post appears.
//! - Re-opening the same workshop surfaces prior posts and lands a
//!   fresh boot post without recreating the seat row.

use std::sync::Arc;
use std::time::Duration;

use ipc::channel_manager::{ATLAS_CHANNEL, ATLAS_SEAT_SPARK_ID};
use ipc::chat_of_record::{TAIL_MAX_LIMIT, TailFilter, tail};
use ipc::irc_client::IrcMessage;
use ipc::lifecycle::{IrcLifecycleConfig, IrcRuntime, LoggingExecutor};
use sqlx::SqlitePool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Minimal IRC server used to verify wire-side effects (JOIN /
/// QUIT). Trimmed copy of the one in `tests/lifecycle.rs` — kept
/// private here so these tests can run independently.
#[derive(Clone)]
struct Mock {
    port: u16,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().unwrap().port();
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let lines_task = Arc::clone(&lines);
        tokio::spawn(async move {
            loop {
                let (sock, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let lines_conn = Arc::clone(&lines_task);
                tokio::spawn(async move {
                    handle_conn(sock, lines_conn).await;
                });
            }
        });
        Self { port, lines }
    }

    async fn lines(&self) -> Vec<String> {
        self.lines.lock().await.clone()
    }

    async fn wait_for<F: Fn(&str) -> bool>(&self, pred: F, budget: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            if let Some(m) = self.lines().await.into_iter().find(|l| pred(l)) {
                return Some(m);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

async fn handle_conn(sock: tokio::net::TcpStream, lines: Arc<Mutex<Vec<String>>>) {
    let (r, mut w) = sock.into_split();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    let mut nick: Option<String> = None;
    let mut user_seen = false;
    let mut welcomed = false;
    loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let _ = n;
        let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
        lines.lock().await.push(trimmed.clone());
        let Some(msg) = IrcMessage::parse(&trimmed) else {
            continue;
        };
        match msg.command.as_str() {
            "NICK" => nick = msg.params.first().cloned(),
            "USER" => user_seen = true,
            "PING" => {
                let token = msg.params.first().cloned().unwrap_or_default();
                let _ = write_line(&mut w, &format!("PONG :{token}")).await;
            }
            "JOIN" => {
                if let (Some(n), Some(ch)) = (&nick, msg.params.first()) {
                    let _ = write_line(&mut w, &format!(":{n}!~{n}@mock JOIN {ch}")).await;
                }
            }
            "QUIT" => {
                let _ = w.shutdown().await;
                return;
            }
            _ => {}
        }
        if !welcomed && user_seen && nick.is_some() {
            let n = nick.as_deref().unwrap();
            let _ = write_line(
                &mut w,
                &format!(":mock.irc 001 {n} :Welcome to the mock IRC server"),
            )
            .await;
            welcomed = true;
        }
    }
}

async fn write_line(w: &mut tokio::net::tcp::OwnedWriteHalf, line: &str) -> std::io::Result<()> {
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\r\n").await?;
    w.flush().await
}

fn lifecycle_config(port: u16) -> IrcLifecycleConfig {
    IrcLifecycleConfig {
        server: "127.0.0.1".into(),
        port,
        tls: false,
        nick: "ryvebot".into(),
        password: None,
        workshop_id: "ws-atlas".into(),
    }
}

async fn tail_atlas(pool: &SqlitePool) -> Vec<String> {
    tail(
        pool,
        TailFilter::for_channel(ATLAS_CHANNEL).with_limit(TAIL_MAX_LIMIT),
    )
    .await
    .expect("tail #atlas")
    .into_iter()
    .map(|m| m.raw_text)
    .collect()
}

/// Re-run the full boot → shutdown flow against the same DB and verify
/// the `#atlas` channel is joined both times, a boot post is written on
/// each launch, and the sentinel seat spark is preserved — the core
/// acceptance for spark ryve-850c0242.
#[sqlx::test(migrations = "../data/migrations")]
async fn atlas_channel_survives_reopen_and_captures_both_boots(pool: SqlitePool) {
    // --- First workshop open --------------------------------------
    let mock1 = Mock::start().await;
    let runtime1 = IrcRuntime::start(
        pool.clone(),
        lifecycle_config(mock1.port),
        Arc::new(LoggingExecutor),
    )
    .await
    .expect("first boot");
    let instance_one = runtime1.atlas_instance_id().to_string();

    let joined_atlas_1 = mock1
        .wait_for(|l| l == "JOIN #atlas", Duration::from_secs(2))
        .await;
    assert!(
        joined_atlas_1.is_some(),
        "first boot must JOIN #atlas; lines: {:?}",
        mock1.lines().await,
    );

    // The sentinel spark must exist so future reads have a stable FK
    // target, regardless of how many real epics live in this workshop.
    let seat_row: (String, String) =
        sqlx::query_as("SELECT id, workshop_id FROM sparks WHERE id = ?")
            .bind(ATLAS_SEAT_SPARK_ID)
            .fetch_one(&pool)
            .await
            .expect("atlas seat sentinel row");
    assert_eq!(seat_row.0, ATLAS_SEAT_SPARK_ID);
    assert_eq!(seat_row.1, "ws-atlas");

    runtime1.shutdown().await;
    let _ = mock1
        .wait_for(|l| l.starts_with("QUIT"), Duration::from_secs(2))
        .await;

    // Post-shutdown DB state: one claim + one release for instance_one.
    let history_after_first = tail_atlas(&pool).await;
    assert_eq!(
        history_after_first.len(),
        2,
        "first launch should leave one boot + one shutdown post, got {:?}",
        history_after_first,
    );
    assert_eq!(
        history_after_first[0],
        format!("atlas {instance_one} online, seat claim"),
    );
    assert_eq!(
        history_after_first[1],
        format!("atlas {instance_one} offline, seat released"),
    );

    // --- Second workshop open against the same DB -----------------
    let mock2 = Mock::start().await;
    let runtime2 = IrcRuntime::start(
        pool.clone(),
        lifecycle_config(mock2.port),
        Arc::new(LoggingExecutor),
    )
    .await
    .expect("second boot");
    let instance_two = runtime2.atlas_instance_id().to_string();
    assert_ne!(
        instance_two, instance_one,
        "every Atlas boot must mint a fresh instance id",
    );

    let joined_atlas_2 = mock2
        .wait_for(|l| l == "JOIN #atlas", Duration::from_secs(2))
        .await;
    assert!(
        joined_atlas_2.is_some(),
        "second boot must JOIN #atlas; lines: {:?}",
        mock2.lines().await,
    );

    // The seat row must not have been duplicated.
    let seat_row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sparks WHERE id = ?")
        .bind(ATLAS_SEAT_SPARK_ID)
        .fetch_one(&pool)
        .await
        .expect("count atlas seat rows");
    assert_eq!(seat_row_count, 1, "atlas seat sentinel must be unique");

    // History carries both boots; the prior launch's posts survived.
    let history_with_both = tail_atlas(&pool).await;
    assert_eq!(
        history_with_both.len(),
        3,
        "expected two boot posts + one release, got {:?}",
        history_with_both,
    );
    assert_eq!(
        history_with_both[0],
        format!("atlas {instance_one} online, seat claim"),
    );
    assert_eq!(
        history_with_both[1],
        format!("atlas {instance_one} offline, seat released"),
    );
    assert_eq!(
        history_with_both[2],
        format!("atlas {instance_two} online, seat claim"),
        "the prior graceful release should let the new boot claim again",
    );

    runtime2.shutdown().await;

    // Final post-shutdown snapshot: both boot posts survived, both
    // releases present, nothing was overwritten.
    let final_history = tail_atlas(&pool).await;
    assert_eq!(final_history.len(), 4);
    assert_eq!(
        final_history[3],
        format!("atlas {instance_two} offline, seat released"),
    );
}

/// A sudden-death launch (boot without a matching release) must make
/// the next Atlas boot as a `follower` — the durable seat-held claim
/// is the source of truth for seat ownership.
#[sqlx::test(migrations = "../data/migrations")]
async fn second_boot_is_follower_when_prior_seat_not_released(pool: SqlitePool) {
    let mock1 = Mock::start().await;
    let runtime1 = IrcRuntime::start(
        pool.clone(),
        lifecycle_config(mock1.port),
        Arc::new(LoggingExecutor),
    )
    .await
    .expect("first boot");
    let instance_one = runtime1.atlas_instance_id().to_string();

    // Simulate sudden-death: drop the runtime without calling
    // `shutdown()` so no seat-released post is written.
    drop(runtime1);

    let mock2 = Mock::start().await;
    let runtime2 = IrcRuntime::start(
        pool.clone(),
        lifecycle_config(mock2.port),
        Arc::new(LoggingExecutor),
    )
    .await
    .expect("second boot");
    let instance_two = runtime2.atlas_instance_id().to_string();

    let history = tail_atlas(&pool).await;
    // Two claims, the second one as a follower because the first
    // never released.
    assert_eq!(history.len(), 2, "{:?}", history);
    assert_eq!(
        history[0],
        format!("atlas {instance_one} online, seat claim"),
    );
    assert_eq!(
        history[1],
        format!("atlas {instance_two} online, seat follower"),
    );

    runtime2.shutdown().await;
}
