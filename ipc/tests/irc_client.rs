// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for `ipc::irc_client` against a local mock IRC server.
//!
//! The mock accepts one connection at a time, responds to NICK/USER with the
//! 001 welcome numeric, echoes JOIN / PART / PRIVMSG back, answers PING, and
//! lets the test force a disconnect to exercise the reconnect path.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ipc::irc_client::{ConnectConfig, IrcClient, IrcMessage};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;

#[derive(Debug, Clone)]
enum MockEvent {
    Connected(u32),
    Line(String),
    ConnectionClosed,
}

enum MockCtrl {
    Drop,
    SendLine(String),
}

struct Mock {
    port: u16,
    events: Arc<Mutex<mpsc::UnboundedReceiver<MockEvent>>>,
    ctrl_tx: Arc<Mutex<Option<mpsc::UnboundedSender<MockCtrl>>>>,
    connection_count: Arc<AtomicUsize>,
}

async fn start_mock() -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let ctrl_slot: Arc<Mutex<Option<mpsc::UnboundedSender<MockCtrl>>>> = Arc::new(Mutex::new(None));
    let ctrl_slot_task = Arc::clone(&ctrl_slot);
    let conn_count = Arc::new(AtomicUsize::new(0));
    let conn_count_task = Arc::clone(&conn_count);

    tokio::spawn(async move {
        loop {
            let (sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let cid = conn_count_task.fetch_add(1, Ordering::SeqCst) as u32 + 1;
            let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<MockCtrl>();
            *ctrl_slot_task.lock().await = Some(ctrl_tx);
            let _ = events_tx.send(MockEvent::Connected(cid));
            handle_conn(sock, events_tx.clone(), &mut ctrl_rx).await;
            let _ = events_tx.send(MockEvent::ConnectionClosed);
            *ctrl_slot_task.lock().await = None;
        }
    });

    Mock {
        port,
        events: Arc::new(Mutex::new(events_rx)),
        ctrl_tx: ctrl_slot,
        connection_count: conn_count,
    }
}

impl Mock {
    async fn next_event(&self) -> Option<MockEvent> {
        timeout(Duration::from_secs(5), self.events.lock().await.recv())
            .await
            .ok()
            .flatten()
    }

    async fn try_next_event(&self, within: Duration) -> Option<MockEvent> {
        timeout(within, self.events.lock().await.recv())
            .await
            .ok()
            .flatten()
    }

    async fn send_ctrl(&self, c: MockCtrl) {
        if let Some(tx) = self.ctrl_tx.lock().await.as_ref() {
            let _ = tx.send(c);
        }
    }

    fn connections(&self) -> usize {
        self.connection_count.load(Ordering::SeqCst)
    }
}

async fn handle_conn(
    sock: tokio::net::TcpStream,
    events: mpsc::UnboundedSender<MockEvent>,
    ctrl_rx: &mut mpsc::UnboundedReceiver<MockCtrl>,
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

            ctrl = ctrl_rx.recv() => {
                match ctrl {
                    Some(MockCtrl::Drop) | None => {
                        let _ = w.shutdown().await;
                        return;
                    }
                    Some(MockCtrl::SendLine(l)) => {
                        if w.write_all(l.as_bytes()).await.is_err() { return; }
                        if w.write_all(b"\r\n").await.is_err() { return; }
                    }
                }
            }

            read = reader.read_line(&mut line) => {
                let n = match read {
                    Ok(0) => return,
                    Ok(n) => n,
                    Err(_) => return,
                };
                let _ = n;
                let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
                let _ = events.send(MockEvent::Line(trimmed.clone()));
                let Some(msg) = IrcMessage::parse(&trimmed) else { continue; };
                match msg.command.as_str() {
                    "NICK" => {
                        nick = msg.params.first().cloned();
                    }
                    "USER" => {
                        user_seen = true;
                    }
                    "PING" => {
                        let token = msg.params.first().cloned().unwrap_or_default();
                        let _ = write_line(&mut w, &format!("PONG :{token}")).await;
                    }
                    "PONG" => {}
                    "JOIN" => {
                        if let (Some(n), Some(ch)) = (&nick, msg.params.first()) {
                            let _ = write_line(&mut w, &format!(":{n}!~{n}@mock JOIN {ch}")).await;
                        }
                    }
                    "PART" => {
                        if let (Some(n), Some(ch)) = (&nick, msg.params.first()) {
                            let _ = write_line(&mut w, &format!(":{n}!~{n}@mock PART {ch}")).await;
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
                    let _ = write_line(&mut w, &format!(":mock.irc 001 {n} :Welcome to the mock IRC server")).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_join_send_receive_disconnect_reconnect() {
    let mock = start_mock().await;

    // Callback channel — keep every non-PING message the client sees.
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<IrcMessage>();
    let cb = Arc::new(move |m: IrcMessage| {
        let _ = msg_tx.send(m);
    });

    let config = ConnectConfig::new("127.0.0.1", mock.port, false, "ryvebot", None);
    let client = IrcClient::connect(config.clone(), cb.clone())
        .await
        .expect("connect");

    // Mock observes the incoming connection.
    match mock.next_event().await {
        Some(MockEvent::Connected(id)) => assert_eq!(id, 1),
        other => panic!("expected Connected(1), got {other:?}"),
    }

    // Drain registration lines (NICK / USER) the mock observed. The 001
    // welcome numeric is consumed internally by `connect()` and never
    // surfaces through the callback, so we don't wait on it.
    drain_events(&mock, Duration::from_millis(200)).await;

    // JOIN #ryve
    client.join("#ryve").await.expect("join");
    let join_echo = wait_for(&mut msg_rx, |m| m.command == "JOIN")
        .await
        .expect("join echo");
    assert_eq!(join_echo.params.first().map(String::as_str), Some("#ryve"));

    // PRIVMSG to #ryve → mock records it.
    client
        .send_privmsg("#ryve", "hello world")
        .await
        .expect("privmsg");

    let sent_line = wait_for_line(&mock, |l| l.starts_with("PRIVMSG #ryve"))
        .await
        .expect("mock saw privmsg");
    assert!(sent_line.contains(":hello world"));

    // Server sends an inbound PRIVMSG → callback observes it.
    mock.send_ctrl(MockCtrl::SendLine(
        ":alice!~a@host PRIVMSG #ryve :hi bot".into(),
    ))
    .await;
    let inbound = wait_for(&mut msg_rx, |m| {
        m.command == "PRIVMSG" && m.params.get(1).is_some_and(|p| p.contains("hi bot"))
    })
    .await
    .expect("inbound privmsg");
    assert_eq!(inbound.prefix.as_deref(), Some("alice!~a@host"));

    // NOTICE, TOPIC → observable via mock.
    client
        .send_notice("#ryve", "notice body")
        .await
        .expect("notice");
    wait_for_line(&mock, |l| l.starts_with("NOTICE #ryve"))
        .await
        .expect("mock saw notice");

    client.set_topic("#ryve", "new topic").await.expect("topic");
    wait_for_line(&mock, |l| l.starts_with("TOPIC #ryve"))
        .await
        .expect("mock saw topic");

    // ChannelNotJoined error path.
    let err = client
        .send_privmsg("#not-joined", "nope")
        .await
        .expect_err("should reject unjoined channel");
    assert!(matches!(
        err,
        ipc::irc_client::IrcError::ChannelNotJoined(_)
    ));

    // --- Force disconnect: mock drops the client's TCP connection. ---
    mock.send_ctrl(MockCtrl::Drop).await;

    // Wait for the mock to observe the connection close.
    let mut saw_close = false;
    for _ in 0..20 {
        if let Some(ev) = mock.try_next_event(Duration::from_millis(500)).await
            && matches!(ev, MockEvent::ConnectionClosed)
        {
            saw_close = true;
            break;
        }
    }
    assert!(saw_close, "mock should see first connection close");

    // Send while disconnected — this should queue and be drained after
    // reconnect, not error immediately.
    client
        .send_privmsg("#ryve", "queued during outage")
        .await
        .expect("queue during outage");

    // Wait for the second Connected event from the mock (auto-reconnect).
    let mut reconnected_id: Option<u32> = None;
    for _ in 0..40 {
        if let Some(ev) = mock.try_next_event(Duration::from_millis(500)).await
            && let MockEvent::Connected(id) = ev
        {
            reconnected_id = Some(id);
            break;
        }
    }
    let reconnected_id = reconnected_id.expect("client auto-reconnects to mock");
    assert_eq!(reconnected_id, 2, "should be the second connection");
    assert_eq!(mock.connections(), 2);

    // After reconnect, the client must auto-rejoin #ryve and drain the
    // queued PRIVMSG.
    let saw_rejoin = wait_for_line(&mock, |l| l == "JOIN #ryve").await.is_some();
    assert!(saw_rejoin, "client should re-JOIN on reconnect");
    let saw_drained = wait_for_line(&mock, |l| {
        l.starts_with("PRIVMSG #ryve") && l.contains("queued during outage")
    })
    .await
    .is_some();
    assert!(saw_drained, "queued PRIVMSG should drain after reconnect");

    // Clean disconnect — client sends QUIT, task exits.
    client.disconnect().await.expect("disconnect");
    let saw_final_close = loop {
        match mock.try_next_event(Duration::from_secs(3)).await {
            Some(MockEvent::ConnectionClosed) => break true,
            Some(_) => continue,
            None => break false,
        }
    };
    assert!(
        saw_final_close,
        "final connection should close after disconnect"
    );
}

async fn drain_events(mock: &Mock, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match timeout(remaining, mock.events.lock().await.recv()).await {
            Ok(Some(_)) => continue,
            _ => return,
        }
    }
}

async fn wait_for<F>(rx: &mut mpsc::UnboundedReceiver<IrcMessage>, pred: F) -> Option<IrcMessage>
where
    F: Fn(&IrcMessage) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match timeout(remaining, rx.recv()).await {
            Ok(Some(m)) => {
                if pred(&m) {
                    return Some(m);
                }
            }
            _ => return None,
        }
    }
}

async fn wait_for_line<F>(mock: &Mock, pred: F) -> Option<String>
where
    F: Fn(&str) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let ev = timeout(remaining, mock.events.lock().await.recv()).await;
        match ev {
            Ok(Some(MockEvent::Line(l))) => {
                if pred(&l) {
                    return Some(l);
                }
            }
            Ok(Some(_)) => continue,
            _ => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_failed_on_unreachable_port() {
    // Bind then drop to get a "guaranteed not listening" port.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let cb: ipc::irc_client::MessageCallback = Arc::new(|_| {});
    let cfg = ConnectConfig::new("127.0.0.1", port, false, "ryvebot", None);
    match IrcClient::connect(cfg, cb).await {
        Err(ipc::irc_client::IrcError::ConnectionFailed(_)) => {}
        Err(e) => panic!("expected ConnectionFailed, got {e:?}"),
        Ok(_) => panic!("expected failure to connect"),
    }
}
