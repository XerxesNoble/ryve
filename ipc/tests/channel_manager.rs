// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for `ipc::channel_manager`.
//!
//! Stands up a small mock IRC server, then drives the channel-manager
//! API end-to-end: create epic → `ensure_channel(client, epic)` → the
//! mock sees the JOIN and the TOPIC. Then `register_actor(client,
//! actor, epic)` is exercised with a second client to confirm the
//! actor joins the same derived channel.

use std::sync::Arc;
use std::time::Duration;

use ipc::channel_manager::{Actor, Epic, channel_name, ensure_channel, register_actor};
use ipc::irc_client::{ConnectConfig, IrcClient, IrcMessage, MessageCallback};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;

struct Mock {
    port: u16,
    lines: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
}

async fn start_mock() -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (lines_tx, lines_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let (sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let tx = lines_tx.clone();
            tokio::spawn(async move { handle_conn(sock, tx).await });
        }
    });

    Mock {
        port,
        lines: Arc::new(Mutex::new(lines_rx)),
    }
}

impl Mock {
    async fn wait_for_line<F>(&self, pred: F) -> Option<String>
    where
        F: Fn(&str) -> bool,
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match timeout(remaining, self.lines.lock().await.recv()).await {
                Ok(Some(l)) if pred(&l) => return Some(l),
                Ok(Some(_)) => continue,
                _ => return None,
            }
        }
    }
}

async fn handle_conn(sock: tokio::net::TcpStream, lines_tx: mpsc::UnboundedSender<String>) {
    let (r, mut w) = sock.into_split();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    let mut nick: Option<String> = None;
    let mut user_seen = false;
    let mut welcomed = false;

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
        let _ = lines_tx.send(trimmed.clone());
        let Some(msg) = IrcMessage::parse(&trimmed) else {
            continue;
        };
        match msg.command.as_str() {
            "NICK" => nick = msg.params.first().cloned(),
            "USER" => user_seen = true,
            "PING" => {
                let token = msg.params.first().cloned().unwrap_or_default();
                let _ = w.write_all(format!("PONG :{token}\r\n").as_bytes()).await;
            }
            "JOIN" => {
                if let (Some(n), Some(ch)) = (&nick, msg.params.first()) {
                    let _ = w
                        .write_all(format!(":{n}!~{n}@mock JOIN {ch}\r\n").as_bytes())
                        .await;
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
            let _ = w
                .write_all(
                    format!(":mock.irc 001 {n} :Welcome to the mock IRC server\r\n").as_bytes(),
                )
                .await;
            welcomed = true;
        }
    }
}

fn no_op_callback() -> MessageCallback {
    Arc::new(|_m: IrcMessage| {})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_channel_joins_and_sets_topic() {
    let mock = start_mock().await;
    let config = ConnectConfig::new("127.0.0.1", mock.port, false, "ryvebot", None);
    let client = IrcClient::connect(config, no_op_callback())
        .await
        .expect("connect");

    let epic = Epic {
        id: "42".into(),
        name: "Checkout Refactor".into(),
        status: "in_progress".into(),
    };

    ensure_channel(&client, &epic)
        .await
        .expect("ensure_channel");

    let expected_channel = channel_name(&epic.as_ref());
    assert_eq!(expected_channel, "#epic-42-checkout-refactor");

    // Mock sees JOIN to the derived channel.
    let join_line = mock
        .wait_for_line(|l| l.starts_with("JOIN ") && l.contains(&expected_channel))
        .await
        .expect("mock saw JOIN for epic channel");
    assert!(join_line.contains("#epic-42-checkout-refactor"));

    // Mock sees TOPIC with name + status payload.
    let topic_line = mock
        .wait_for_line(|l| l.starts_with("TOPIC ") && l.contains(&expected_channel))
        .await
        .expect("mock saw TOPIC for epic channel");
    assert!(topic_line.contains("Checkout Refactor"));
    assert!(topic_line.contains("in_progress"));

    client.disconnect().await.expect("disconnect");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_channel_is_idempotent() {
    let mock = start_mock().await;
    let config = ConnectConfig::new("127.0.0.1", mock.port, false, "ryvebot", None);
    let client = IrcClient::connect(config, no_op_callback())
        .await
        .expect("connect");

    let epic = Epic {
        id: "99".into(),
        name: "Idempotent Flow".into(),
        status: "open".into(),
    };

    // Two back-to-back calls must not explode; the effect on the
    // server is "still joined, topic still set" either way.
    ensure_channel(&client, &epic).await.expect("first call");
    ensure_channel(&client, &epic).await.expect("second call");

    let expected_channel = channel_name(&epic.as_ref());
    assert!(
        mock.wait_for_line(|l| l.starts_with("JOIN ") && l.contains(&expected_channel))
            .await
            .is_some(),
        "mock must observe at least one JOIN"
    );
    assert!(
        mock.wait_for_line(|l| l.starts_with("TOPIC ") && l.contains(&expected_channel))
            .await
            .is_some(),
        "mock must observe at least one TOPIC"
    );

    client.disconnect().await.expect("disconnect");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_actor_joins_the_channel() {
    let mock = start_mock().await;
    let config = ConnectConfig::new("127.0.0.1", mock.port, false, "actor1", None);
    let client = IrcClient::connect(config, no_op_callback())
        .await
        .expect("connect");

    let epic = Epic {
        id: "7".into(),
        name: "Auth Rewrite".into(),
        status: "open".into(),
    };
    let actor = Actor::new("agent_claude_01");

    register_actor(&client, &actor, &epic.as_ref())
        .await
        .expect("register_actor");

    let expected_channel = channel_name(&epic.as_ref());
    let join_line = mock
        .wait_for_line(|l| l.starts_with("JOIN ") && l.contains(&expected_channel))
        .await
        .expect("mock saw JOIN from actor client");
    assert!(join_line.contains("#epic-7-auth-rewrite"));

    client.disconnect().await.expect("disconnect");
}
