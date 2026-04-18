// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for `ipc::outbox_relay`.
//!
//! Spins up a minimal mock IRC server, populates `event_outbox`, runs the
//! relay through a single drain pass, and asserts the end-to-end pipeline:
//! drain → filter → render → send → persist → mark.

use std::sync::Arc;
use std::time::Duration;

use ipc::irc_client::{ConnectConfig, IrcClient, IrcMessage};
use ipc::outbox_relay::{RelayConfig, RelayHandle};
use sqlx::SqlitePool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;

/// Captured IRC line seen by the mock server (either inbound or emitted
/// during welcome handshake).
#[derive(Debug, Clone)]
struct MockCapture {
    lines: Arc<Mutex<Vec<String>>>,
}

impl MockCapture {
    async fn snapshot(&self) -> Vec<String> {
        self.lines.lock().await.clone()
    }

    async fn privmsgs(&self) -> Vec<(String, String)> {
        self.snapshot()
            .await
            .into_iter()
            .filter_map(|line| {
                let msg = IrcMessage::parse(&line)?;
                if msg.command != "PRIVMSG" {
                    return None;
                }
                let target = msg.params.first()?.clone();
                let text = msg.params.get(1)?.clone();
                Some((target, text))
            })
            .collect()
    }
}

/// Mock IRC server that accepts one client, welcomes it, echoes JOINs,
/// and captures every line seen on the wire. Sufficient to observe the
/// relay's PRIVMSGs without depending on the full irc_client test rig.
async fn start_mock() -> (u16, MockCapture) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    let lines = Arc::new(Mutex::new(Vec::new()));
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

    (port, MockCapture { lines })
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

/// Seed an epic spark so `irc_messages.epic_id` FK is satisfiable.
async fn seed_epic(pool: &SqlitePool, id: &str) {
    sqlx::query(
        "INSERT INTO sparks \
         (id, title, description, status, priority, spark_type, workshop_id, \
          created_at, updated_at) \
         VALUES (?, 'Epic', '', 'open', 1, 'epic', 'ws-test', \
                 '2026-04-15T09:00:00Z', '2026-04-15T09:00:00Z')",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_outbox_row(
    pool: &SqlitePool,
    event_id: &str,
    timestamp: &str,
    event_type: &str,
    payload: serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, \
          event_type, payload) \
         VALUES (?, 1, ?, 'asgn-1', 'actor-1', ?, ?)",
    )
    .bind(event_id)
    .bind(timestamp)
    .bind(event_type)
    .bind(payload.to_string())
    .execute(pool)
    .await
    .unwrap();
}

async fn state_row(pool: &SqlitePool, event_id: &str) -> Option<(String, i64, Option<String>)> {
    sqlx::query_as::<_, (String, i64, Option<String>)>(
        "SELECT status, attempts, last_error FROM irc_outbox_state \
         WHERE event_id = ?",
    )
    .bind(event_id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// Connect an [`IrcClient`] to the mock on `port` and wait for the
/// welcome handshake before returning.
async fn connect_client(port: u16) -> Arc<IrcClient> {
    let (msg_tx, _msg_rx) = mpsc::unbounded_channel::<IrcMessage>();
    let cb = Arc::new(move |m: IrcMessage| {
        let _ = msg_tx.send(m);
    });
    let config = ConnectConfig::new("127.0.0.1", port, false, "ryvebot", None);
    let client = IrcClient::connect(config, cb).await.expect("irc connect");
    // Give the server time to process NICK/USER and reply with 001 so
    // subsequent JOINs are accepted without race.
    tokio::time::sleep(Duration::from_millis(100)).await;
    Arc::new(client)
}

async fn wait_for_privmsg_count(
    capture: &MockCapture,
    expected: usize,
    budget: Duration,
) -> Vec<(String, String)> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let privmsgs = capture.privmsgs().await;
        if privmsgs.len() >= expected {
            return privmsgs;
        }
        if tokio::time::Instant::now() >= deadline {
            return privmsgs;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[sqlx::test(migrations = "../data/migrations")]
async fn relay_drain_filters_renders_sends_persists_and_marks(pool: SqlitePool) {
    seed_epic(&pool, "epic-1").await;
    let (port, capture) = start_mock().await;
    let client = connect_client(port).await;

    let channel = "#epic-epic-1-checkout";
    // The client must have joined the channel before the relay sends a
    // PRIVMSG to it, otherwise the client rejects with ChannelNotJoined.
    client.join(channel).await.expect("join");
    // Drain the welcome handshake so our later assertions can count
    // just the relay-emitted PRIVMSGs.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let allowed_payload_1 = serde_json::json!({
        "epic_id": "epic-1",
        "epic_name": "Checkout",
        "assignment_id": "asgn-1",
        "actor": "alice",
    });
    let allowed_payload_2 = serde_json::json!({
        "epic_id": "epic-1",
        "epic_name": "Checkout",
        "assignment_id": "asgn-1",
        "reason": "waiting on review",
    });
    let heartbeat_payload = serde_json::json!({
        "epic_id": "epic-1",
        "epic_name": "Checkout",
    });

    insert_outbox_row(
        &pool,
        "evt-1",
        "2026-04-15T10:00:00Z",
        "assignment.created",
        allowed_payload_1,
    )
    .await;
    insert_outbox_row(
        &pool,
        "evt-2",
        "2026-04-15T10:00:01Z",
        "assignment.heartbeat",
        heartbeat_payload,
    )
    .await;
    insert_outbox_row(
        &pool,
        "evt-3",
        "2026-04-15T10:00:02Z",
        "assignment.stuck",
        allowed_payload_2,
    )
    .await;

    let config = RelayConfig {
        poll_interval: Duration::from_millis(5),
        max_attempts: 5,
        batch_size: 10,
        workshop_id: "ws-test".to_string(),
    };
    let relay = RelayHandle::new(pool.clone(), Arc::clone(&client), config);

    let outcome = relay.drain_once().await.expect("drain");
    assert_eq!(outcome.fetched, 3, "outcome: {outcome:?}");
    assert_eq!(outcome.sent, 2, "outcome: {outcome:?}");
    assert_eq!(outcome.skipped_filtered, 1, "outcome: {outcome:?}");
    assert_eq!(outcome.failed, 0, "outcome: {outcome:?}");

    // 2 IRC PRIVMSGs on the wire — heartbeat was never sent.
    let privmsgs = wait_for_privmsg_count(&capture, 2, Duration::from_secs(2)).await;
    assert_eq!(privmsgs.len(), 2, "privmsgs: {privmsgs:?}");
    for (target, _) in &privmsgs {
        assert_eq!(target, channel, "target mismatch: {privmsgs:?}");
    }

    // irc_messages has exactly 2 persisted rows.
    let irc_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM irc_messages WHERE epic_id = ?")
        .bind("epic-1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(irc_count, 2);

    // All 3 outbox rows are marked 'sent' in irc_outbox_state.
    for event_id in ["evt-1", "evt-2", "evt-3"] {
        let state = state_row(&pool, event_id).await;
        assert!(state.is_some(), "no state row for {event_id}");
        let (status, _attempts, last_error) = state.unwrap();
        assert_eq!(status, "sent", "event {event_id} state: {status:?}");
        assert!(
            last_error.is_none(),
            "event {event_id} last_error: {last_error:?}"
        );
    }

    // Second pass is a no-op — no more outbox rows in the relay's view.
    let second = relay.drain_once().await.expect("second drain");
    assert_eq!(second.fetched, 0);
    assert_eq!(second.sent, 0);
    assert_eq!(second.skipped_filtered, 0);
    assert_eq!(second.failed, 0);

    // Explicit cleanup: close the client so the task shuts down.
    let _ = timeout(Duration::from_secs(1), client.disconnect()).await;
}

#[sqlx::test(migrations = "../data/migrations")]
async fn send_failure_marks_failed_and_retries_until_flare(pool: SqlitePool) {
    // No IRC client at all — every send will fail because the channel
    // is not joined, exercising the failure path deterministically
    // without needing to break a live socket.
    seed_epic(&pool, "epic-2").await;
    let (port, _capture) = start_mock().await;
    let client = connect_client(port).await;
    // Deliberately do NOT join the target channel — `send_privmsg` will
    // return `ChannelNotJoined` every time.

    insert_outbox_row(
        &pool,
        "evt-1",
        "2026-04-15T10:00:00Z",
        "assignment.created",
        serde_json::json!({
            "epic_id": "epic-2",
            "epic_name": "Inventory",
            "assignment_id": "asgn-42",
            "actor": "bob",
        }),
    )
    .await;

    let relay = RelayHandle::new(
        pool.clone(),
        Arc::clone(&client),
        RelayConfig {
            poll_interval: Duration::from_millis(5),
            max_attempts: 3,
            batch_size: 10,
            workshop_id: "ws-test".to_string(),
        },
    );

    // Pass 1: failure, attempts = 1, status stays 'pending' so the next
    // cycle picks the row up again, no flare yet.
    let p1 = relay.drain_once().await.expect("pass 1");
    assert_eq!(p1.failed, 1);
    assert_eq!(p1.flared, 0);
    let (status, attempts, last_error) = state_row(&pool, "evt-1").await.unwrap();
    assert_eq!(status, "pending");
    assert_eq!(attempts, 1);
    assert!(
        last_error.as_deref().unwrap().contains("irc send failed"),
        "last_error: {last_error:?}"
    );

    // Pass 2: still under max_attempts → row is re-fetched, attempts = 2,
    // status remains 'pending', still no flare.
    let p2 = relay.drain_once().await.expect("pass 2");
    assert_eq!(p2.fetched, 1);
    assert_eq!(p2.failed, 1);
    assert_eq!(p2.flared, 0);
    let (status, attempts, _) = state_row(&pool, "evt-1").await.unwrap();
    assert_eq!(status, "pending");
    assert_eq!(attempts, 2);

    // Pass 3: attempts now reaches max_attempts (3) → row transitions to
    // the terminal 'failed' state and a flare ember is emitted.
    let p3 = relay.drain_once().await.expect("pass 3");
    assert_eq!(p3.failed, 1);
    assert_eq!(p3.flared, 1);
    let (status, attempts, _) = state_row(&pool, "evt-1").await.unwrap();
    assert_eq!(status, "failed");
    assert_eq!(attempts, 3);

    // Pass 4: row is now terminal 'failed' and excluded from fetch_pending.
    // The relay does no further work on it without operator intervention.
    let p4 = relay.drain_once().await.expect("pass 4");
    assert_eq!(p4.fetched, 0);
    assert_eq!(p4.failed, 0);
    assert_eq!(p4.flared, 0);

    // Exactly one flare ember landed (only on the max_attempts cliff).
    let flares: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM embers WHERE workshop_id = ? AND ember_type = 'flare'",
    )
    .bind("ws-test")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(flares, 1);

    let _ = timeout(Duration::from_secs(1), client.disconnect()).await;
}
