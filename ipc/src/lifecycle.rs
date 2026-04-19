// SPDX-License-Identifier: AGPL-3.0-or-later

//! IRC lifecycle — wires the client, channel manager, outbox relay, and
//! inbound /ryve listener into the app's boot/shutdown flow.
//!
//! A single workshop owns at most one [`IrcRuntime`]. [`IrcRuntime::start`]
//! performs the opt-in boot sequence:
//!
//! 1. **Connect** the [`IrcClient`](crate::irc_client::IrcClient) using the
//!    workshop's settings (server/port/tls/nick/password).
//! 2. **Ensure a channel per open epic** via
//!    [`channel_manager::ensure_channel`]. Idempotent — reconnecting later
//!    re-runs the JOIN from the client's own cache.
//! 3. **Start the outbox relay** as a background task. The task loops
//!    [`RelayHandle::drain_once`] with a `poll_interval` sleep, shutdown-
//!    aware via a oneshot.
//! 4. **Activate the inbound listener**: the message callback passed to
//!    `IrcClient::connect` parses incoming PRIVMSGs through
//!    [`irc_command_parser::dispatch`] and emits the resulting reply (if
//!    any) back on the same client. The listener runs inside the client —
//!    no separate task required.
//!
//! The runtime is default-on per workshop (spark ryve-300f661c
//! [sp-31659bbb]): `WorkshopConfig::irc_enabled` returns `true` by
//! default and [`WorkshopConfig::effective_irc_server_address`] resolves
//! to the bundled `127.0.0.1:<irc_bundled_port>` daemon unless the user
//! sets an explicit `irc_server` override (e.g. `irc.libera.chat` for a
//! mesh setup). The app never constructs an [`IrcRuntime`] when
//! `irc_enabled` is explicitly disabled or when neither a bundled port
//! nor an override is configured (pre-`ryve init` workshops).
//!
//! Shutdown via [`IrcRuntime::shutdown`] is clean:
//! 1. Signal the relay task via oneshot so it stops sleeping.
//! 2. One last `drain_once` flushes anything that queued up between the
//!    final scheduled drain and the stop signal.
//! 3. [`IrcClient::disconnect`] sends `QUIT :bye` and awaits the session
//!    task so no connection is left orphaned.

use std::sync::Arc;

use data::ryve_dir::WorkshopConfig;
use data::sparks::types::{EmberType, NewEmber};
use data::sparks::{ember_repo, spark_repo};
use sqlx::SqlitePool;
use thiserror::Error;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::channel_manager::{self, Epic};
use crate::irc_client::{ConnectConfig, IrcClient, IrcError, IrcMessage, MessageCallback};
use crate::irc_command_parser::{
    self, CommandExecutor, DispatchOutcome, ExecError, ExecFuture, IrcReplyKind, ReviewDecision,
    StatusSnapshot,
};
use crate::outbox_relay::{RelayConfig, RelayHandle};

/// Errors surfaced by [`IrcRuntime::start`]. All variants are non-fatal —
/// the caller is expected to emit a flare ember (see [`IrcRuntime::emit_connect_flare`])
/// and continue running the rest of the workshop without IRC.
#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error(
        "IRC is not enabled for this workshop (irc_enabled = false or no server_address resolvable)"
    )]
    Disabled,

    #[error("IRC connect failed: {0}")]
    Connect(#[source] IrcError),

    #[error("IRC channel bootstrap failed: {0}")]
    Channel(#[source] IrcError),

    #[error("database error while listing epics: {0}")]
    Database(#[from] data::sparks::error::SparksError),
}

/// Configuration snapshot extracted from [`WorkshopConfig`] at boot time.
/// Kept as an owned struct so the lifecycle doesn't carry a reference into
/// the workshop config — the boot task is async and the config may be
/// re-read from disk before the task completes.
#[derive(Debug, Clone)]
pub struct IrcLifecycleConfig {
    pub server: String,
    pub port: u16,
    pub tls: bool,
    pub nick: String,
    pub password: Option<String>,
    pub workshop_id: String,
}

impl IrcLifecycleConfig {
    /// Extract IRC settings from a [`WorkshopConfig`]. Returns `None`
    /// when the workshop has IRC explicitly disabled (`irc_enabled =
    /// false`) or when no server address resolves — i.e. the workshop
    /// has not yet been inited (no bundled port recorded) and no
    /// explicit `irc_server` override is set. In either case the caller
    /// should skip the IRC boot path.
    ///
    /// Spark ryve-300f661c [sp-31659bbb]: the earlier `irc_server.clone()?`
    /// short-circuit is gone — the runtime now dials the bundled
    /// `127.0.0.1:<irc_bundled_port>` daemon whenever an override isn't
    /// set, via [`WorkshopConfig::effective_irc_server_address`].
    pub fn from_workshop(config: &WorkshopConfig, workshop_id: impl Into<String>) -> Option<Self> {
        if !config.irc_enabled() {
            return None;
        }
        let address = config.effective_irc_server_address()?;
        let (server, port) = split_host_port(&address)?;
        Some(Self {
            server,
            port,
            tls: config.irc_tls.unwrap_or(false),
            nick: config.effective_irc_nick(),
            password: config.irc_password.clone(),
            workshop_id: workshop_id.into(),
        })
    }
}

/// Parse a `host:port` pair produced by
/// [`WorkshopConfig::effective_irc_server_address`]. Uses `rsplit_once`
/// so IPv6 literals without an explicit port degrade predictably — the
/// helper only emits `host:port` today but future overrides may include
/// more exotic forms, and callers should treat an unparseable address
/// the same as "no address".
fn split_host_port(addr: &str) -> Option<(String, u16)> {
    let (host, port) = addr.rsplit_once(':')?;
    let port = port.parse::<u16>().ok()?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

/// Owner of the per-workshop IRC subsystem. Holds the client, the relay
/// shutdown handle, and the inbound executor.
pub struct IrcRuntime {
    client: Arc<IrcClient>,
    stop_tx: Option<oneshot::Sender<()>>,
    relay_join: Option<JoinHandle<()>>,
}

impl IrcRuntime {
    /// Connect, ensure channels for every open epic in the workshop, and
    /// spawn the outbox relay. The inbound listener is wired into the
    /// client's message callback so no extra task is required.
    ///
    /// On connect failure the returned `Err(LifecycleError::Connect)` gives
    /// the caller the chance to emit its own flare. The helper
    /// [`IrcRuntime::emit_connect_flare`] is the canonical way to do that.
    pub async fn start(
        pool: SqlitePool,
        config: IrcLifecycleConfig,
        executor: Arc<dyn CommandExecutor>,
    ) -> Result<Self, LifecycleError> {
        let open_epics = list_open_epics(&pool, &config.workshop_id).await?;
        Self::start_with_epics(pool, config, open_epics, executor).await
    }

    /// Variant of [`IrcRuntime::start`] that takes the list of epics
    /// directly. Used by tests to avoid seeding the sparks table just to
    /// prove the lifecycle joins one channel per epic. Production callers
    /// should use [`IrcRuntime::start`].
    pub async fn start_with_epics(
        pool: SqlitePool,
        config: IrcLifecycleConfig,
        open_epics: Vec<Epic>,
        executor: Arc<dyn CommandExecutor>,
    ) -> Result<Self, LifecycleError> {
        let client_slot: Arc<Mutex<Option<Arc<IrcClient>>>> = Arc::new(Mutex::new(None));
        let callback: MessageCallback =
            build_inbound_callback(executor, Arc::clone(&client_slot), config.nick.clone());

        let connect_cfg = ConnectConfig::new(
            config.server.clone(),
            config.port,
            config.tls,
            config.nick.clone(),
            config.password.clone(),
        );
        let client = IrcClient::connect(connect_cfg, callback)
            .await
            .map_err(LifecycleError::Connect)?;
        let client = Arc::new(client);
        *client_slot.lock().await = Some(Arc::clone(&client));

        for epic in &open_epics {
            channel_manager::ensure_channel(&client, epic)
                .await
                .map_err(LifecycleError::Channel)?;
        }

        let relay_config = RelayConfig {
            workshop_id: config.workshop_id.clone(),
            ..RelayConfig::default()
        };
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let relay_join = spawn_relay_loop(pool.clone(), Arc::clone(&client), relay_config, stop_rx);

        Ok(Self {
            client,
            stop_tx: Some(stop_tx),
            relay_join: Some(relay_join),
        })
    }

    /// Handle to the live IRC client. Exposed primarily for tests and for
    /// UI surfaces that want to observe connection state; the relay and
    /// the channel manager already own their own references.
    pub fn client(&self) -> &Arc<IrcClient> {
        &self.client
    }

    /// Make sure the channel for `epic` exists on the server and the
    /// topic is current. Called on epic creation and on epic renames;
    /// idempotent by construction.
    pub async fn ensure_epic_channel(&self, epic: &Epic) -> Result<(), IrcError> {
        channel_manager::ensure_channel(&self.client, epic).await
    }

    /// Signal the relay task to stop, let it drain one last time, then
    /// send QUIT on the client. Awaits the relay task's join handle so
    /// no mid-drain write is abandoned.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.relay_join.take() {
            match join.await {
                Ok(()) => {}
                Err(e) if e.is_cancelled() => {}
                Err(e) => log::warn!("irc relay task panicked during shutdown: {e}"),
            }
        }
        if let Err(e) = self.client.disconnect().await {
            log::warn!("irc client disconnect reported error: {e}");
        }
    }

    /// Persist a `flare` ember announcing that the IRC server was
    /// unreachable at boot. Called by the app when [`IrcRuntime::start`]
    /// returns `Err(LifecycleError::Connect)`.
    pub async fn emit_connect_flare(pool: &SqlitePool, workshop_id: &str, reason: &str) {
        if let Err(e) = ember_repo::create(
            pool,
            NewEmber {
                ember_type: EmberType::Flare,
                content: format!("IRC unavailable at boot: {reason}"),
                source_agent: Some("irc_lifecycle".to_string()),
                workshop_id: workshop_id.to_string(),
                ttl_seconds: None,
            },
        )
        .await
        {
            log::warn!("failed to persist IRC connect flare ember: {e}");
        }
    }
}

fn build_inbound_callback(
    executor: Arc<dyn CommandExecutor>,
    client_slot: Arc<Mutex<Option<Arc<IrcClient>>>>,
    own_nick: String,
) -> MessageCallback {
    Arc::new(move |msg: IrcMessage| {
        if msg.command != "PRIVMSG" {
            return;
        }
        let Some(target) = msg.params.first().cloned() else {
            return;
        };
        let Some(body) = msg.params.get(1).cloned() else {
            return;
        };
        let sender = msg
            .prefix
            .as_deref()
            .and_then(|p| p.split('!').next())
            .unwrap_or("")
            .to_string();
        // Don't react to our own echo — otherwise the bot can loop on its
        // own replies if the server reflects them back.
        if sender == own_nick {
            return;
        }
        let channel = if channel_manager::IRC_MAX_CHANNEL_LEN > 0 && target.starts_with('#')
            || target.starts_with('&')
        {
            target
        } else {
            // Private message — reply target is the sender's nick.
            sender.clone()
        };

        let executor = Arc::clone(&executor);
        let client_slot = Arc::clone(&client_slot);
        tokio::spawn(async move {
            let outcome =
                irc_command_parser::dispatch(executor.as_ref(), &sender, &channel, &body).await;
            let reply = match outcome {
                DispatchOutcome::Ignored | DispatchOutcome::Handled { reply: None } => return,
                DispatchOutcome::Handled { reply: Some(reply) }
                | DispatchOutcome::Rejected { reply } => reply,
            };
            let client = {
                let guard = client_slot.lock().await;
                guard.as_ref().map(Arc::clone)
            };
            let Some(client) = client else { return };
            let send_result = match reply.kind {
                IrcReplyKind::Privmsg => client.send_privmsg(&reply.target, &reply.body).await,
                IrcReplyKind::Notice => client.send_notice(&reply.target, &reply.body).await,
            };
            if let Err(e) = send_result {
                log::warn!("irc inbound reply failed: {e}");
            }
        });
    })
}

fn spawn_relay_loop(
    pool: SqlitePool,
    client: Arc<IrcClient>,
    config: RelayConfig,
    stop_rx: oneshot::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let poll_interval = config.poll_interval;
        let handle = RelayHandle::new(pool, client, config);
        let mut stop_rx = stop_rx;
        loop {
            tokio::select! {
                biased;
                _ = &mut stop_rx => {
                    if let Err(e) = handle.drain_once().await {
                        log::warn!("irc relay: final drain failed: {e}");
                    }
                    return;
                }
                _ = tokio::time::sleep(poll_interval) => {}
            }
            if let Err(e) = handle.drain_once().await {
                log::warn!("irc relay: drain failed: {e}");
            }
        }
    })
}

async fn list_open_epics(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<Vec<Epic>, data::sparks::error::SparksError> {
    use data::sparks::types::{SparkFilter, SparkStatus, SparkType};

    let sparks = spark_repo::list(
        pool,
        SparkFilter {
            workshop_id: Some(workshop_id.to_string()),
            spark_type: Some(SparkType::Epic),
            status: Some(vec![
                SparkStatus::Open,
                SparkStatus::InProgress,
                SparkStatus::Blocked,
            ]),
            ..Default::default()
        },
    )
    .await?;
    Ok(sparks
        .into_iter()
        .map(|s| Epic {
            id: s.id,
            name: s.title,
            status: s.status,
        })
        .collect())
}

/// Placeholder [`CommandExecutor`] that logs every inbound `/ryve` command
/// and refuses to mutate state — safe default to plug into the lifecycle
/// until a production executor ships.
///
/// Real executors go through the same transition validator / outbox path
/// the programmatic API uses. Spark ryve-5a0e1d97 intentionally ships
/// without that path so the lifecycle can land independently; the parser
/// (ryve-f1891f82) is already merged and the `CommandExecutor` seam is
/// the extension point.
pub struct LoggingExecutor;

impl CommandExecutor for LoggingExecutor {
    fn transition<'a>(
        &'a self,
        sender: &'a str,
        asg_id: &'a str,
        target_phase: &'a str,
        expected_phase: &'a str,
    ) -> ExecFuture<'a, ()> {
        Box::pin(async move {
            log::info!(
                "irc inbound /ryve transition from {sender}: asg={asg_id} \
                 target={target_phase} expected={expected_phase} (not wired)"
            );
            Err(ExecError::Internal(
                "mutations via IRC are not wired in v1".into(),
            ))
        })
    }

    fn review<'a>(
        &'a self,
        sender: &'a str,
        asg_id: &'a str,
        decision: ReviewDecision,
        summary: Option<&'a str>,
    ) -> ExecFuture<'a, ()> {
        Box::pin(async move {
            log::info!(
                "irc inbound /ryve review from {sender}: asg={asg_id} \
                 decision={} summary={:?} (not wired)",
                decision.as_str(),
                summary
            );
            Err(ExecError::Internal(
                "mutations via IRC are not wired in v1".into(),
            ))
        })
    }

    fn blocker<'a>(
        &'a self,
        sender: &'a str,
        asg_id: &'a str,
        reason: &'a str,
    ) -> ExecFuture<'a, ()> {
        Box::pin(async move {
            log::info!(
                "irc inbound /ryve blocker from {sender}: asg={asg_id} reason={reason} (not wired)"
            );
            Err(ExecError::Internal(
                "mutations via IRC are not wired in v1".into(),
            ))
        })
    }

    fn status<'a>(&'a self, asg_id: &'a str) -> ExecFuture<'a, StatusSnapshot> {
        Box::pin(async move {
            log::info!("irc inbound /ryve status: asg={asg_id} (not wired)");
            Err(ExecError::Internal(
                "status over IRC is not wired in v1".into(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_workshop_returns_none_when_explicitly_disabled() {
        // Spark ryve-300f661c: the master switch still opts the
        // workshop out entirely.
        let cfg = WorkshopConfig {
            irc_enabled: false,
            irc_bundled_port: Some(6971),
            ..Default::default()
        };
        assert!(IrcLifecycleConfig::from_workshop(&cfg, "ws").is_none());
    }

    #[test]
    fn from_workshop_returns_none_without_override_or_bundled_port() {
        // Pre-`ryve init` workshop: nothing to dial. Enabled but no
        // address resolves, so the runtime is skipped cleanly.
        let cfg = WorkshopConfig::default();
        assert!(cfg.irc_enabled());
        assert!(IrcLifecycleConfig::from_workshop(&cfg, "ws").is_none());
    }

    #[test]
    fn from_workshop_uses_bundled_port_when_no_override() {
        // Default-on path: `ryve init` has recorded a bundled port and
        // the user has no `irc_server` override. The lifecycle dials
        // the workshop-local daemon on 127.0.0.1:<bundled_port>.
        let cfg = WorkshopConfig {
            irc_bundled_port: Some(6971),
            ..Default::default()
        };
        let lc = IrcLifecycleConfig::from_workshop(&cfg, "ws")
            .expect("bundled port resolves to a dialable address");
        assert_eq!(lc.server, "127.0.0.1");
        assert_eq!(lc.port, 6971);
        assert!(!lc.tls);
        assert_eq!(lc.nick, "ryve");
        assert!(lc.password.is_none());
        assert_eq!(lc.workshop_id, "ws");
    }

    #[test]
    fn from_workshop_explicit_override_wins_over_bundled() {
        // Mesh/cross-workshop setup: an explicit `irc_server` plus port
        // trumps the bundled daemon even if a bundled port is set.
        let cfg = WorkshopConfig {
            irc_server: Some("irc.libera.chat".into()),
            irc_port: Some(6697),
            irc_tls: Some(true),
            irc_bundled_port: Some(6971),
            ..Default::default()
        };
        let lc = IrcLifecycleConfig::from_workshop(&cfg, "ws").expect("explicit override resolves");
        assert_eq!(lc.server, "irc.libera.chat");
        assert_eq!(lc.port, 6697);
        assert!(lc.tls);
    }

    #[test]
    fn from_workshop_populates_defaults() {
        let cfg = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            ..Default::default()
        };
        let lc = IrcLifecycleConfig::from_workshop(&cfg, "ws").unwrap();
        assert_eq!(lc.server, "irc.example.com");
        assert_eq!(lc.port, 6667);
        assert!(!lc.tls);
        assert_eq!(lc.nick, "ryve");
        assert!(lc.password.is_none());
        assert_eq!(lc.workshop_id, "ws");
    }

    #[test]
    fn from_workshop_honours_tls_defaults() {
        let cfg = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            irc_tls: Some(true),
            irc_nick: Some("bot".into()),
            irc_password: Some("pw".into()),
            ..Default::default()
        };
        let lc = IrcLifecycleConfig::from_workshop(&cfg, "ws").unwrap();
        assert_eq!(lc.port, 6697);
        assert!(lc.tls);
        assert_eq!(lc.nick, "bot");
        assert_eq!(lc.password.as_deref(), Some("pw"));
    }

    #[test]
    fn split_host_port_parses_basic_ipv4() {
        let (host, port) = split_host_port("127.0.0.1:6971").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 6971);
    }

    #[test]
    fn split_host_port_rejects_unparseable() {
        assert!(split_host_port("no-colon").is_none());
        assert!(split_host_port(":6971").is_none());
        assert!(split_host_port("host:notaport").is_none());
    }
}
