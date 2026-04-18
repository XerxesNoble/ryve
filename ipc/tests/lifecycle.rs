// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for `ipc::lifecycle` — the IRC app-lifecycle glue
//! that connects the client, ensures a channel per open epic, and starts
//! the outbox relay.
//!
//! Spark ryve-5a0e1d97 [sp-ddf6fd7f].

use std::sync::Arc;
use std::time::Duration;

use ipc::channel_manager::Epic;
use ipc::irc_client::IrcMessage;
use ipc::irc_command_parser::{
    CommandExecutor, ExecError, ExecFuture, ReviewDecision, StatusSnapshot,
};
use ipc::lifecycle::{IrcLifecycleConfig, IrcRuntime, LifecycleError, LoggingExecutor};
use sqlx::SqlitePool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Minimal mock IRC server: captures every wire line, answers NICK+USER
/// with the 001 welcome, echoes JOINs, and QUITs cleanly. Also exposes a
/// control channel so tests can push synthetic inbound lines onto the
/// active connection (used to prove the inbound `/ryve` listener is
/// wired into the client's message callback).
#[derive(Clone)]
struct Mock {
    port: u16,
    lines: Arc<Mutex<Vec<String>>>,
    inject_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>>,
}

impl Mock {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().unwrap().port();
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let inject_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>> =
            Arc::new(Mutex::new(None));
        let lines_task = Arc::clone(&lines);
        let inject_task = Arc::clone(&inject_tx);
        tokio::spawn(async move {
            loop {
                let (sock, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let lines_conn = Arc::clone(&lines_task);
                let inject_slot = Arc::clone(&inject_task);
                tokio::spawn(async move {
                    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                    *inject_slot.lock().await = Some(tx);
                    handle_conn(sock, lines_conn, rx).await;
                    *inject_slot.lock().await = None;
                });
            }
        });
        Self {
            port,
            lines,
            inject_tx,
        }
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

    async fn inject(&self, line: String) {
        if let Some(tx) = self.inject_tx.lock().await.as_ref() {
            let _ = tx.send(line);
        }
    }
}

async fn handle_conn(
    sock: tokio::net::TcpStream,
    lines: Arc<Mutex<Vec<String>>>,
    mut inject_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let (r, mut w) = sock.into_split();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    let mut nick: Option<String> = None;
    let mut user_seen = false;
    let mut welcomed = false;
    loop {
        line.clear();
        tokio::select! {
            biased;
            inject = inject_rx.recv() => {
                match inject {
                    Some(l) => {
                        if write_line(&mut w, &l).await.is_err() {
                            return;
                        }
                    }
                    None => return,
                }
            }
            n = reader.read_line(&mut line) => {
                let n = match n {
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
    }
}

async fn write_line(w: &mut tokio::net::tcp::OwnedWriteHalf, line: &str) -> std::io::Result<()> {
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\r\n").await?;
    w.flush().await
}

async fn seed_epic(pool: &SqlitePool, id: &str, title: &str, status: &str) {
    sqlx::query(
        "INSERT INTO sparks \
         (id, title, description, status, priority, spark_type, workshop_id, \
          created_at, updated_at) \
         VALUES (?, ?, '', ?, 1, 'epic', 'ws-test', \
                 '2026-04-15T09:00:00Z', '2026-04-15T09:00:00Z')",
    )
    .bind(id)
    .bind(title)
    .bind(status)
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

fn lifecycle_config(port: u16, nick: &str) -> IrcLifecycleConfig {
    IrcLifecycleConfig {
        server: "127.0.0.1".into(),
        port,
        tls: false,
        nick: nick.into(),
        password: None,
        workshop_id: "ws-test".into(),
    }
}

/// Acceptance criterion: on app boot with IRC configured, the client
/// connects, channels are ensured for every open epic, the relay drains
/// pending outbox events, and shutdown closes the connection cleanly.
#[sqlx::test(migrations = "../data/migrations")]
async fn boot_connects_joins_channels_and_runs_relay(pool: SqlitePool) {
    seed_epic(&pool, "epic-1", "Checkout", "open").await;
    seed_epic(&pool, "epic-2", "Billing", "in_progress").await;
    // Closed epics must NOT get a channel — the spark's acceptance says
    // "channels for all open epics" only.
    seed_epic(&pool, "epic-3", "Retired", "closed").await;

    let mock = Mock::start().await;
    let cfg = lifecycle_config(mock.port, "ryvebot");

    let runtime = IrcRuntime::start(pool.clone(), cfg, Arc::new(LoggingExecutor))
        .await
        .expect("runtime boots");

    // Client connected — mock observed the NICK line.
    let nick_line = mock
        .wait_for(|l| l.starts_with("NICK "), Duration::from_secs(2))
        .await
        .expect("client sent NICK");
    assert!(nick_line.contains("ryvebot"));

    // Two open epics → two JOINs for the canonical channel names. The
    // closed one is not joined.
    let joined_open_1 = mock
        .wait_for(
            |l| l == "JOIN #epic-epic-1-checkout",
            Duration::from_secs(2),
        )
        .await;
    assert!(joined_open_1.is_some(), "expected JOIN for open epic-1");

    let joined_open_2 = mock
        .wait_for(|l| l == "JOIN #epic-epic-2-billing", Duration::from_secs(2))
        .await;
    assert!(joined_open_2.is_some(), "expected JOIN for open epic-2");

    let joined_closed = mock
        .lines()
        .await
        .into_iter()
        .find(|l| l.contains("#epic-epic-3"));
    assert!(
        joined_closed.is_none(),
        "closed epics must not be joined, saw: {joined_closed:?}"
    );

    // Relay is running: insert an allow-listed event and verify it
    // lands on the wire as a PRIVMSG within a reasonable drain window.
    insert_outbox_row(
        &pool,
        "evt-boot",
        "2026-04-15T10:00:00Z",
        "assignment.created",
        serde_json::json!({
            "epic_id": "epic-1",
            "epic_name": "Checkout",
            "assignment_id": "asgn-1",
            "actor": "alice",
        }),
    )
    .await;

    let privmsg_seen = mock
        .wait_for(
            |l| l.starts_with("PRIVMSG #epic-epic-1-checkout"),
            Duration::from_secs(3),
        )
        .await;
    assert!(
        privmsg_seen.is_some(),
        "relay should drain the outbox row onto IRC within the window"
    );

    // Shutdown is clean: the client sends QUIT.
    runtime.shutdown().await;
    let quit_seen = mock
        .wait_for(|l| l.starts_with("QUIT"), Duration::from_secs(2))
        .await;
    assert!(quit_seen.is_some(), "client must send QUIT on shutdown");
}

/// Acceptance criterion: epic-creation auto-creates the channel. The app
/// layer observes new epics on SparksLoaded and calls `ensure_epic_channel`;
/// here we exercise the runtime method that performs the JOIN.
#[sqlx::test(migrations = "../data/migrations")]
async fn ensure_epic_channel_joins_new_epic(pool: SqlitePool) {
    // No epics at boot — the runtime comes up clean.
    let mock = Mock::start().await;
    let cfg = lifecycle_config(mock.port, "ryvebot");
    let runtime = IrcRuntime::start(pool.clone(), cfg, Arc::new(LoggingExecutor))
        .await
        .expect("runtime boots");
    // Drain the connect handshake.
    tokio::time::sleep(Duration::from_millis(150)).await;

    runtime
        .ensure_epic_channel(&Epic {
            id: "epic-new".into(),
            name: "Fresh Epic".into(),
            status: "open".into(),
        })
        .await
        .expect("ensure_epic_channel");

    let joined = mock
        .wait_for(
            |l| l == "JOIN #epic-epic-new-fresh-epic",
            Duration::from_secs(2),
        )
        .await;
    assert!(joined.is_some(), "expected JOIN for newly-created epic");

    runtime.shutdown().await;
}

/// Invariant: if the IRC server is unreachable at boot, [`IrcRuntime::start`]
/// returns a connect error AND [`IrcRuntime::emit_connect_flare`] writes a
/// flare ember, so the app can keep running without IRC.
#[sqlx::test(migrations = "../data/migrations")]
async fn unreachable_server_persists_flare_ember(pool: SqlitePool) {
    let unused_port = find_unused_port().await;
    let cfg = lifecycle_config(unused_port, "ryvebot");
    let result = IrcRuntime::start(pool.clone(), cfg, Arc::new(LoggingExecutor)).await;
    let err = match result {
        Ok(_) => panic!("connect should fail against a closed port"),
        Err(e) => e,
    };
    assert!(matches!(err, LifecycleError::Connect(_)), "got {err:?}");

    IrcRuntime::emit_connect_flare(&pool, "ws-test", &err.to_string()).await;

    let (ember_type, content): (String, String) = sqlx::query_as(
        "SELECT ember_type, content FROM embers WHERE workshop_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind("ws-test")
    .fetch_one(&pool)
    .await
    .expect("flare ember present");
    assert_eq!(ember_type, "flare");
    assert!(content.contains("IRC unavailable at boot"));
}

/// Invariant: inbound `/ryve` command parsing is wired into the client's
/// message callback — a PRIVMSG that reaches the bot is parsed and handed
/// to the injected `CommandExecutor`. Free-text PRIVMSGs are silently
/// ignored, so the executor only gets called when a `/ryve` line arrives.
#[sqlx::test(migrations = "../data/migrations")]
async fn inbound_ryve_command_reaches_executor(pool: SqlitePool) {
    let mock = Mock::start().await;

    // Spying executor that records every incoming command on an mpsc so
    // the test can await the first observed call.
    struct Spy {
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    }

    impl CommandExecutor for Spy {
        fn transition<'a>(
            &'a self,
            sender: &'a str,
            asg_id: &'a str,
            target_phase: &'a str,
            _expected_phase: &'a str,
        ) -> ExecFuture<'a, ()> {
            let _ = self
                .tx
                .send(format!("transition {sender} {asg_id} {target_phase}"));
            Box::pin(async move { Err(ExecError::Internal("stub".into())) })
        }
        fn review<'a>(
            &'a self,
            _sender: &'a str,
            _asg_id: &'a str,
            _decision: ReviewDecision,
            _summary: Option<&'a str>,
        ) -> ExecFuture<'a, ()> {
            Box::pin(async move { Err(ExecError::Internal("stub".into())) })
        }
        fn blocker<'a>(
            &'a self,
            _sender: &'a str,
            _asg_id: &'a str,
            _reason: &'a str,
        ) -> ExecFuture<'a, ()> {
            Box::pin(async move { Err(ExecError::Internal("stub".into())) })
        }
        fn status<'a>(&'a self, _asg_id: &'a str) -> ExecFuture<'a, StatusSnapshot> {
            Box::pin(async move { Err(ExecError::Internal("stub".into())) })
        }
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let cfg = lifecycle_config(mock.port, "ryvebot");
    let runtime = IrcRuntime::start(pool, cfg, Arc::new(Spy { tx }))
        .await
        .expect("runtime boots");

    // Wait for the welcome handshake so subsequent JOINs are accepted.
    let _ = mock
        .wait_for(|l| l.starts_with("USER "), Duration::from_secs(2))
        .await;
    runtime.client().join("#ops").await.expect("join");
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Free-text chatter must NOT reach the executor.
    mock.inject(":alice!~a@host PRIVMSG #ops :hi everyone".into())
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        rx.try_recv().is_err(),
        "free-text PRIVMSG should not invoke the executor"
    );

    // A `/ryve transition` line should reach the spy.
    mock.inject(
        ":alice!~a@host PRIVMSG #ops :/ryve transition asgn-123 in_progress expected=assigned"
            .into(),
    )
    .await;
    let observed = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("executor must receive /ryve command")
        .expect("executor must receive /ryve command");
    assert_eq!(observed, "transition alice asgn-123 in_progress");

    runtime.shutdown().await;
}

async fn find_unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}
