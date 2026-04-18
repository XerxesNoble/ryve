// SPDX-License-Identifier: AGPL-3.0-or-later

//! Async IRC client.
//!
//! Thin, standalone IRC client: connect (plain or TLS), register, join / part
//! channels, send PRIVMSG / NOTICE / TOPIC, handle PING / PONG internally,
//! and recover from disconnects with bounded exponential backoff. Messages
//! the user submits while the connection is down are queued and drained on
//! reconnect.
//!
//! The module knows nothing about Ryve events, outbox, or rendering — it
//! speaks raw IRC only. The caller receives parsed [`IrcMessage`]s through a
//! user-supplied callback.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Callback invoked for every parsed message received from the server
/// (PING frames are handled internally and not forwarded).
pub type MessageCallback = Arc<dyn Fn(IrcMessage) + Send + Sync + 'static>;

/// Typed errors surfaced to the caller.
#[derive(Debug, Error)]
pub enum IrcError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    #[error("not joined to channel: {0}")]
    ChannelNotJoined(String),
    #[error("send failed: {0}")]
    SendFailed(String),
    #[error("disconnected")]
    Disconnected,
}

/// Configuration for a single IRC connection.
#[derive(Debug, Clone)]
pub struct ConnectConfig {
    pub server: String,
    pub port: u16,
    pub tls: bool,
    pub nick: String,
    pub password: Option<String>,
}

impl ConnectConfig {
    pub fn new(
        server: impl Into<String>,
        port: u16,
        tls: bool,
        nick: impl Into<String>,
        password: Option<String>,
    ) -> Self {
        Self {
            server: server.into(),
            port,
            tls,
            nick: nick.into(),
            password,
        }
    }
}

/// A parsed IRC wire message.
///
/// Covers the subset Ryve needs: optional `:prefix`, an uppercased command
/// (verb or three-digit numeric), and space-separated params with the final
/// `:trailing` param collapsed into a single entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrcMessage {
    pub prefix: Option<String>,
    pub command: String,
    pub params: Vec<String>,
}

impl IrcMessage {
    /// Parse one wire line. Returns `None` for an empty / whitespace-only line.
    pub fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim_end_matches(['\r', '\n']).trim_start();
        if trimmed.is_empty() {
            return None;
        }
        let mut rest = trimmed;
        let prefix = if let Some(after) = rest.strip_prefix(':') {
            let (p, r) = after.split_once(' ')?;
            rest = r.trim_start();
            Some(p.to_string())
        } else {
            None
        };
        let (cmd, params_str) = match rest.split_once(' ') {
            Some((c, r)) => (c, r.trim_start()),
            None => (rest, ""),
        };
        if cmd.is_empty() {
            return None;
        }
        let command = cmd.to_uppercase();

        let mut params = Vec::new();
        let mut remaining = params_str;
        while !remaining.is_empty() {
            if let Some(trailing) = remaining.strip_prefix(':') {
                params.push(trailing.to_string());
                break;
            }
            match remaining.split_once(' ') {
                Some((p, rem)) => {
                    params.push(p.to_string());
                    remaining = rem.trim_start();
                }
                None => {
                    params.push(remaining.to_string());
                    break;
                }
            }
        }

        Some(IrcMessage {
            prefix,
            command,
            params,
        })
    }
}

/// Command from the public API to the background session task.
enum Command {
    Join(String),
    Part(String),
    Privmsg(String, String),
    Notice(String, String),
    Topic(String, String),
    Disconnect,
}

/// Async IRC client.
///
/// Call [`IrcClient::disconnect`] (or drop the client) to tear down the
/// background session task.
pub struct IrcClient {
    cmd_tx: mpsc::UnboundedSender<Command>,
    joined: Arc<Mutex<HashSet<String>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl IrcClient {
    /// Connect, register, and spawn the background session task.
    ///
    /// Returns `Err(ConnectionFailed)` if the TCP (or TLS) handshake fails,
    /// or `Err(AuthFailed)` if the server rejects registration before the
    /// welcome numeric.
    pub async fn connect(
        config: ConnectConfig,
        on_message: MessageCallback,
    ) -> Result<Self, IrcError> {
        let session = establish_and_register(&config).await?;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let joined = Arc::new(Mutex::new(HashSet::<String>::new()));
        let handle = tokio::spawn(session_loop(
            config,
            session,
            cmd_rx,
            on_message,
            Arc::clone(&joined),
        ));
        Ok(Self {
            cmd_tx,
            joined,
            handle: Mutex::new(Some(handle)),
        })
    }

    /// Request to join a channel. The join is also recorded locally so the
    /// session task can auto-rejoin on reconnect.
    pub async fn join(&self, channel: &str) -> Result<(), IrcError> {
        validate_channel(channel)?;
        self.joined.lock().await.insert(channel.to_string());
        self.cmd_tx
            .send(Command::Join(channel.to_string()))
            .map_err(|_| IrcError::Disconnected)?;
        Ok(())
    }

    /// Leave a channel.
    pub async fn part(&self, channel: &str) -> Result<(), IrcError> {
        validate_channel(channel)?;
        self.joined.lock().await.remove(channel);
        self.cmd_tx
            .send(Command::Part(channel.to_string()))
            .map_err(|_| IrcError::Disconnected)?;
        Ok(())
    }

    /// Send a PRIVMSG. Channel targets (starting with `#` or `&`) must have
    /// been joined; direct messages to a nick are allowed unconditionally.
    pub async fn send_privmsg(&self, target: &str, text: &str) -> Result<(), IrcError> {
        if is_channel(target) && !self.joined.lock().await.contains(target) {
            return Err(IrcError::ChannelNotJoined(target.to_string()));
        }
        self.cmd_tx
            .send(Command::Privmsg(target.to_string(), text.to_string()))
            .map_err(|_| IrcError::Disconnected)?;
        Ok(())
    }

    /// Send a NOTICE. Same target rules as [`IrcClient::send_privmsg`].
    pub async fn send_notice(&self, target: &str, text: &str) -> Result<(), IrcError> {
        if is_channel(target) && !self.joined.lock().await.contains(target) {
            return Err(IrcError::ChannelNotJoined(target.to_string()));
        }
        self.cmd_tx
            .send(Command::Notice(target.to_string(), text.to_string()))
            .map_err(|_| IrcError::Disconnected)?;
        Ok(())
    }

    /// Set (or request) the topic on a channel we have joined.
    pub async fn set_topic(&self, channel: &str, text: &str) -> Result<(), IrcError> {
        validate_channel(channel)?;
        if !self.joined.lock().await.contains(channel) {
            return Err(IrcError::ChannelNotJoined(channel.to_string()));
        }
        self.cmd_tx
            .send(Command::Topic(channel.to_string(), text.to_string()))
            .map_err(|_| IrcError::Disconnected)?;
        Ok(())
    }

    /// Send QUIT and tear down the session task. Idempotent.
    pub async fn disconnect(&self) -> Result<(), IrcError> {
        let _ = self.cmd_tx.send(Command::Disconnect);
        if let Some(handle) = self.handle.lock().await.take() {
            let _ = handle.await;
        }
        Ok(())
    }
}

impl Drop for IrcClient {
    fn drop(&mut self) {
        // Best-effort tear-down; callers that care about a clean QUIT should
        // `disconnect().await` before dropping.
        let _ = self.cmd_tx.send(Command::Disconnect);
    }
}

fn is_channel(target: &str) -> bool {
    target.starts_with('#') || target.starts_with('&')
}

fn validate_channel(channel: &str) -> Result<(), IrcError> {
    if !is_channel(channel) {
        return Err(IrcError::SendFailed(format!(
            "invalid channel name: {channel}"
        )));
    }
    Ok(())
}

// ---------- session internals ----------

/// Object-safe trait alias so plain and TLS streams share one boxed type.
trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + ?Sized> AsyncStream for T {}

type BoxedStream = Box<dyn AsyncStream>;
type Reader = BufReader<ReadHalf<BoxedStream>>;
type Writer = WriteHalf<BoxedStream>;

struct Session {
    reader: Reader,
    writer: Writer,
}

enum SessionOutcome {
    /// User called [`IrcClient::disconnect`] — stop permanently.
    Disconnect,
    /// I/O failure mid-session — reconnect with backoff.
    ConnectionLost,
}

async fn session_loop(
    config: ConnectConfig,
    initial: Session,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    on_message: MessageCallback,
    joined: Arc<Mutex<HashSet<String>>>,
) {
    const INITIAL_BACKOFF: Duration = Duration::from_millis(250);

    let mut session: Option<Session> = Some(initial);
    let mut backoff = INITIAL_BACKOFF;
    // Commands consumed from cmd_rx but not yet confirmed written to a live
    // connection. Survives across session losses so the drain-after-reconnect
    // invariant holds even when a write races a socket close.
    let mut pending: VecDeque<Command> = VecDeque::new();

    loop {
        let active = match session.take() {
            Some(s) => s,
            None => match reconnect(&config, &joined, &mut backoff).await {
                Some(s) => s,
                // reconnect() slept for `backoff` on failure — loop to retry.
                None => continue,
            },
        };
        backoff = INITIAL_BACKOFF;

        match run_session(active, &mut cmd_rx, &on_message, &mut pending).await {
            SessionOutcome::Disconnect => return,
            SessionOutcome::ConnectionLost => {
                // Fall through: session is None → next iteration reconnects.
            }
        }
    }
}

async fn reconnect(
    config: &ConnectConfig,
    joined: &Arc<Mutex<HashSet<String>>>,
    backoff: &mut Duration,
) -> Option<Session> {
    match establish_and_register(config).await {
        Ok(mut s) => {
            let channels: Vec<String> = joined.lock().await.iter().cloned().collect();
            for ch in channels {
                if write_irc(&mut s.writer, &format!("JOIN {ch}"))
                    .await
                    .is_err()
                {
                    return None;
                }
            }
            Some(s)
        }
        Err(_) => {
            tokio::time::sleep(*backoff).await;
            *backoff = (*backoff * 2).min(Duration::from_secs(60));
            None
        }
    }
}

async fn run_session(
    mut session: Session,
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
    on_message: &MessageCallback,
    pending: &mut VecDeque<Command>,
) -> SessionOutcome {
    // Self-originated PING keeps idle connections alive through NAT and lets
    // us detect half-open sockets when the server stops responding.
    let ping_interval = Duration::from_secs(120);
    let mut next_ping = Instant::now() + ping_interval;
    let mut line = String::new();

    // Flush any commands that outlived the previous connection. If a write
    // fails mid-drain, put the offender back at the front so the next
    // reconnect retries in order.
    while let Some(cmd) = pending.pop_front() {
        match cmd {
            Command::Disconnect => {
                let _ = write_irc(&mut session.writer, "QUIT :bye").await;
                let _ = session.writer.shutdown().await;
                return SessionOutcome::Disconnect;
            }
            other => {
                let wire = render_command(&other);
                if write_irc(&mut session.writer, &wire).await.is_err() {
                    pending.push_front(other);
                    return SessionOutcome::ConnectionLost;
                }
            }
        }
    }

    // The most recent command we wrote out on *this* session. TCP write_all
    // returning Ok means the bytes landed in the kernel buffer, not that the
    // peer received them — if the connection then drops, the bytes are lost
    // silently. Any server activity (Ok(n>0) on read) clears this, since it
    // proves prior writes made it past the kernel. If the session dies
    // without such proof, the still-pending command is requeued so the
    // next reconnect redelivers it, preserving drain-after-reconnect.
    let mut last_unconfirmed: Option<Command> = None;

    loop {
        line.clear();
        tokio::select! {
            // Poll the reader first so an already-ready EOF is seen before we
            // consume a fresh command from cmd_rx. Without this, both arms can
            // be ready at the same time (peer closed + API push) and the
            // command would be "written" into a dead TCP buffer and silently
            // lost, violating the drain-after-reconnect invariant.
            biased;

            read = session.reader.read_line(&mut line) => {
                match read {
                    Ok(0) => {
                        if let Some(c) = last_unconfirmed.take() {
                            pending.push_front(c);
                        }
                        return SessionOutcome::ConnectionLost;
                    }
                    Ok(_) => {
                        // Any inbound byte from the server proves the socket
                        // round-trip is healthy: prior writes reached the
                        // peer, so the in-flight command is confirmed.
                        last_unconfirmed = None;
                        if let Some(msg) = IrcMessage::parse(&line) {
                            if msg.command == "PING" {
                                let token = msg.params.first().cloned().unwrap_or_default();
                                if write_irc(&mut session.writer, &format!("PONG :{token}"))
                                    .await
                                    .is_err()
                                {
                                    return SessionOutcome::ConnectionLost;
                                }
                            } else {
                                (on_message)(msg);
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(c) = last_unconfirmed.take() {
                            pending.push_front(c);
                        }
                        return SessionOutcome::ConnectionLost;
                    }
                }
            }

            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    // Sender dropped — no more API calls possible.
                    let _ = write_irc(&mut session.writer, "QUIT :bye").await;
                    let _ = session.writer.shutdown().await;
                    return SessionOutcome::Disconnect;
                };
                match cmd {
                    Command::Disconnect => {
                        let _ = write_irc(&mut session.writer, "QUIT :bye").await;
                        let _ = session.writer.shutdown().await;
                        return SessionOutcome::Disconnect;
                    }
                    other => {
                        // Fast-path requeue if the peer has already closed;
                        // avoids an unnecessary at-least-once retry on the
                        // common race where cmd_rx and reader are both ready.
                        if reader_has_eof(&mut session.reader).await {
                            if let Some(c) = last_unconfirmed.take() {
                                pending.push_front(c);
                            }
                            pending.push_back(other);
                            return SessionOutcome::ConnectionLost;
                        }
                        let wire = render_command(&other);
                        if write_irc(&mut session.writer, &wire).await.is_err() {
                            if let Some(c) = last_unconfirmed.take() {
                                pending.push_front(c);
                            }
                            pending.push_back(other);
                            return SessionOutcome::ConnectionLost;
                        }
                        last_unconfirmed = Some(other);
                    }
                }
            }

            _ = tokio::time::sleep_until(next_ping) => {
                if write_irc(&mut session.writer, "PING :ryve-keepalive")
                    .await
                    .is_err()
                {
                    if let Some(c) = last_unconfirmed.take() {
                        pending.push_front(c);
                    }
                    return SessionOutcome::ConnectionLost;
                }
                next_ping = Instant::now() + ping_interval;
            }
        }
    }
}

fn render_command(cmd: &Command) -> String {
    match cmd {
        Command::Join(ch) => format!("JOIN {ch}"),
        Command::Part(ch) => format!("PART {ch}"),
        Command::Privmsg(t, msg) => format!("PRIVMSG {t} :{msg}"),
        Command::Notice(t, msg) => format!("NOTICE {t} :{msg}"),
        Command::Topic(ch, topic) => format!("TOPIC {ch} :{topic}"),
        // Disconnect is handled in-place by run_session.
        Command::Disconnect => String::from("QUIT :bye"),
    }
}

async fn write_irc<W: AsyncWrite + Unpin + ?Sized>(
    writer: &mut W,
    line: &str,
) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\r\n").await?;
    writer.flush().await
}

/// Poll the reader for an already-delivered EOF (or error) without consuming
/// buffered data. Returns `true` when the peer has hung up and the caller
/// should treat the connection as dead before issuing further writes.
///
/// `fill_buf` only drives the underlying `poll_read` when the internal buffer
/// is empty; if the peer has already sent FIN, the next poll yields an empty
/// slice (or an error) synchronously. We race that against `yield_now` so a
/// connection with no EOF and no buffered data returns `false` immediately
/// instead of blocking.
async fn reader_has_eof(reader: &mut Reader) -> bool {
    tokio::select! {
        biased;

        filled = reader.fill_buf() => {
            match filled {
                Ok([]) => true,
                Ok(_) => false,
                Err(_) => true,
            }
        }

        _ = tokio::task::yield_now() => false,
    }
}

async fn establish_and_register(config: &ConnectConfig) -> Result<Session, IrcError> {
    let addr = format!("{}:{}", config.server, config.port);
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| IrcError::ConnectionFailed(e.to_string()))?;
    let _ = tcp.set_nodelay(true);

    let stream: BoxedStream = if config.tls {
        Box::new(tls_wrap(tcp, &config.server).await?)
    } else {
        Box::new(tcp)
    };

    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    if let Some(password) = &config.password {
        write_irc(&mut writer, &format!("PASS {password}"))
            .await
            .map_err(|e| IrcError::ConnectionFailed(e.to_string()))?;
    }
    write_irc(&mut writer, &format!("NICK {}", config.nick))
        .await
        .map_err(|e| IrcError::ConnectionFailed(e.to_string()))?;
    write_irc(
        &mut writer,
        &format!("USER {} 0 * :{}", config.nick, config.nick),
    )
    .await
    .map_err(|e| IrcError::ConnectionFailed(e.to_string()))?;

    // Read until 001 (RPL_WELCOME) or a registration error numeric. Bounded
    // so a silent server never wedges connect().
    let registration_timeout = Duration::from_secs(20);
    let deadline = Instant::now() + registration_timeout;
    let mut line = String::new();
    loop {
        line.clear();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(IrcError::AuthFailed(
                "timed out waiting for server welcome".into(),
            ));
        }
        let read = tokio::time::timeout(remaining, reader.read_line(&mut line)).await;
        match read {
            Ok(Ok(0)) => {
                return Err(IrcError::ConnectionFailed(
                    "server closed during registration".into(),
                ));
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(IrcError::ConnectionFailed(e.to_string())),
            Err(_) => {
                return Err(IrcError::AuthFailed(
                    "timed out waiting for server welcome".into(),
                ));
            }
        }
        let Some(msg) = IrcMessage::parse(&line) else {
            continue;
        };
        match msg.command.as_str() {
            "001" => break,
            // Nick-in-use, erroneous nickname, collision, no nickname,
            // bad password, banned, need-password.
            "431" | "432" | "433" | "436" | "437" | "464" | "465" | "475" => {
                return Err(IrcError::AuthFailed(msg.params.join(" ")));
            }
            "PING" => {
                let token = msg.params.first().cloned().unwrap_or_default();
                write_irc(&mut writer, &format!("PONG :{token}"))
                    .await
                    .map_err(|e| IrcError::ConnectionFailed(e.to_string()))?;
            }
            _ => {} // ignore informational lines (NOTICE, CAP, etc.)
        }
    }

    Ok(Session { reader, writer })
}

async fn tls_wrap(
    tcp: TcpStream,
    server: &str,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, IrcError> {
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::pki_types::ServerName;
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};

    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from(server.to_string())
        .map_err(|e| IrcError::ConnectionFailed(format!("invalid TLS server name: {e}")))?;
    connector
        .connect(name, tcp)
        .await
        .map_err(|e| IrcError::ConnectionFailed(format!("TLS handshake failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_command() {
        let m = IrcMessage::parse("PING :server.example\r\n").unwrap();
        assert_eq!(m.prefix, None);
        assert_eq!(m.command, "PING");
        assert_eq!(m.params, vec!["server.example"]);
    }

    #[test]
    fn parses_prefixed_privmsg_with_trailing() {
        let m = IrcMessage::parse(":alice!~a@host PRIVMSG #chan :hello world\r\n").unwrap();
        assert_eq!(m.prefix.as_deref(), Some("alice!~a@host"));
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.params, vec!["#chan", "hello world"]);
    }

    #[test]
    fn parses_numeric_with_many_params() {
        let m = IrcMessage::parse(":irc.example 001 ryve :Welcome to the Internet Relay Network")
            .unwrap();
        assert_eq!(m.command, "001");
        assert_eq!(
            m.params,
            vec!["ryve", "Welcome to the Internet Relay Network"]
        );
    }

    #[test]
    fn empty_line_parses_to_none() {
        assert!(IrcMessage::parse("").is_none());
        assert!(IrcMessage::parse("\r\n").is_none());
        assert!(IrcMessage::parse("   ").is_none());
    }

    #[test]
    fn command_is_uppercased() {
        let m = IrcMessage::parse("privmsg #a :hi").unwrap();
        assert_eq!(m.command, "PRIVMSG");
    }

    #[test]
    fn channel_detection_rules() {
        assert!(is_channel("#foo"));
        assert!(is_channel("&local"));
        assert!(!is_channel("alice"));
    }

    #[test]
    fn validate_channel_rejects_nick() {
        assert!(matches!(
            validate_channel("alice"),
            Err(IrcError::SendFailed(_))
        ));
        assert!(validate_channel("#foo").is_ok());
    }

    #[test]
    fn render_command_shapes() {
        assert_eq!(render_command(&Command::Join("#a".into())), "JOIN #a");
        assert_eq!(render_command(&Command::Part("#a".into())), "PART #a");
        assert_eq!(
            render_command(&Command::Privmsg("#a".into(), "hi".into())),
            "PRIVMSG #a :hi"
        );
        assert_eq!(
            render_command(&Command::Notice("bob".into(), "hey".into())),
            "NOTICE bob :hey"
        );
        assert_eq!(
            render_command(&Command::Topic("#a".into(), "subj".into())),
            "TOPIC #a :subj"
        );
    }
}
