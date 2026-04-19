// SPDX-License-Identifier: AGPL-3.0-or-later

//! Process supervisor for the workshop-scoped ngIRCd daemon.
//!
//! Every Ryve launch starts (or reconciles to) a local IRC server on the
//! workshop-allocated port so agent-to-agent coordination has a working
//! backbone out of the box. This module owns that process: spawning the
//! bundled daemon against the workshop's `.ryve/ircd/ircd.conf`, writing a
//! pidfile so the next launch can adopt the same daemon instead of
//! spawning a duplicate, and shutting the process down with
//! SIGTERM-then-SIGKILL on graceful app exit.
//!
//! The supervisor is deliberately thin:
//!
//! * [`SpawnSpec::for_workshop`] resolves the bundled binary
//!   ([`crate::bundled_ircd::bundled_ircd_path`]), reads the bundled port
//!   recorded by `ryve init` into
//!   [`data::ryve_dir::WorkshopConfig::irc_bundled_port`], and points at
//!   the workshop-scoped config file. It returns `None` when any piece is
//!   missing — the binary isn't built, the workshop hasn't been
//!   `ryve init`-ed yet, or the config file hasn't been written. Callers
//!   treat that as "no IRC daemon for this workshop" and skip the
//!   supervisor entirely.
//! * [`IrcdSupervisor::start`] performs the reconcile-then-spawn dance:
//!   an already-running daemon (live pidfile or port probe) is adopted
//!   without a new spawn so two back-to-back launches produce one daemon,
//!   not two. Otherwise it spawns ngIRCd in `--nodaemon` mode (so the
//!   [`tokio::process::Child`] stays tied to our handle) and records a
//!   pidfile for the next launch.
//! * [`IrcdSupervisor::shutdown`] sends SIGTERM, waits up to
//!   [`SHUTDOWN_GRACE`], then escalates to SIGKILL. Only runs when we
//!   spawned the child ourselves; a reconciled supervisor leaves the
//!   daemon alive so the next launch can re-adopt it.
//!
//! The workshop-scoped config file itself is written by `ryve init`
//! (spark ryve-4d5881c2) — this module only supervises what's already on
//! disk and skips cleanly when it isn't.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use data::ryve_dir::{RyveDir, WorkshopConfig};
use tokio::process::{Child, Command};

use crate::bundled_ircd::bundled_ircd_path;

/// Time we wait between sending SIGTERM and escalating to SIGKILL.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Upper bound on how long [`IrcdSupervisor::start`] will poll the port
/// for the freshly-spawned daemon before returning. Hitting the ceiling
/// is non-fatal — the daemon may still come up a moment later and the
/// IRC lifecycle retries the connect on its own.
const PORT_READY_TIMEOUT: Duration = Duration::from_millis(3000);

/// Relative path (under `.ryve/`) of the per-workshop ngIRCd config file
/// the supervisor points the daemon at.
pub const IRCD_CONFIG_RELATIVE: &str = "ircd/ircd.conf";

/// Relative path (under `.ryve/`) of the per-workshop pidfile the
/// supervisor writes and reads for reconcile.
pub const IRCD_PIDFILE_RELATIVE: &str = "ircd/ircd.pid";

/// Full path to the workshop-scoped ngIRCd config file.
pub fn ircd_config_path(ryve_dir: &RyveDir) -> PathBuf {
    ryve_dir.root().join(IRCD_CONFIG_RELATIVE)
}

/// Full path to the workshop-scoped pidfile used for reconciliation.
pub fn ircd_pidfile_path(ryve_dir: &RyveDir) -> PathBuf {
    ryve_dir.root().join(IRCD_PIDFILE_RELATIVE)
}

/// Errors the supervisor surfaces to callers. All variants are non-fatal
/// at the app level — the caller logs and proceeds without IRC for this
/// boot, mirroring the IRC-lifecycle flare behaviour.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("io while managing ngircd process: {0}")]
    Io(#[from] std::io::Error),
}

/// Everything the supervisor needs to spawn a daemon. Kept as a plain
/// struct so tests can construct an instance pointing at a stub binary
/// (e.g. `/bin/sleep`) without having to stand up a real workshop.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub binary: PathBuf,
    pub args: Vec<OsString>,
    pub pidfile: PathBuf,
    /// Port the supervisor will probe to detect an already-running
    /// daemon and to wait-for-ready after spawn. Even when the stub
    /// binary under test doesn't actually bind, keeping the port in the
    /// spec lets the port-probe reconcile path stay on the same
    /// structural signature as production.
    pub port: u16,
}

impl SpawnSpec {
    /// Build the production spawn spec for this workshop. Returns
    /// `None` when any required input is missing so the caller can
    /// cleanly skip the supervisor on a pre-`ryve init` workshop, on a
    /// platform without the bundled binary, or before
    /// `scripts/build-vendored-ircd.sh` has run.
    ///
    /// PR #50 Copilot c2: also returns `None` when the user has
    /// explicitly disabled IRC via `config.irc_enabled() == false`.
    /// Previously a workshop that had a bundled port + ircd.conf from
    /// a prior `ryve init` would still spawn / reconcile ngIRCd on
    /// startup even after the user flipped the opt-out, leaving a
    /// background daemon running unexpectedly.
    pub fn for_workshop(ryve_dir: &RyveDir, config: &WorkshopConfig) -> Option<Self> {
        if !config.irc_enabled() {
            return None;
        }
        let binary = bundled_ircd_path()?;
        let port = config.irc_bundled_port?;
        let config_path = ircd_config_path(ryve_dir);
        if !config_path.exists() {
            return None;
        }
        Some(Self {
            binary,
            args: vec![
                OsString::from("--config"),
                OsString::from(config_path.as_os_str()),
                OsString::from("--nodaemon"),
            ],
            pidfile: ircd_pidfile_path(ryve_dir),
            port,
        })
    }
}

/// The mode in which the supervisor is running. Drives shutdown
/// behaviour: an adopted daemon is left running so the next launch can
/// reconcile it again, while an owned child is taken down gracefully on
/// app exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorMode {
    /// This supervisor spawned the daemon. `shutdown` sends SIGTERM and
    /// escalates to SIGKILL on timeout, then removes the pidfile.
    Owned,
    /// This supervisor reconciled to a daemon started by a previous
    /// launch (or an externally managed ngircd on the same port).
    /// `shutdown` is a no-op — the daemon outlives us.
    Adopted,
}

/// Per-workshop supervisor handle. Held on the iced-app side for the
/// lifetime of the workshop so `shutdown` can drive the graceful
/// SIGTERM-then-SIGKILL sequence on close.
pub struct IrcdSupervisor {
    pidfile: PathBuf,
    pid: u32,
    mode: SupervisorMode,
    /// `Some(Child)` iff `mode == Owned`. The handle is kept so
    /// `shutdown` can `wait()` the reaped process instead of leaving a
    /// zombie after SIGTERM.
    owned_child: Option<Child>,
    port: u16,
}

impl IrcdSupervisor {
    /// Reconcile-then-spawn entry point. Mirrors the bundled-tmux
    /// resolver's "check, then fall back" shape:
    ///
    /// 1. If the pidfile points at a live PID, adopt that daemon.
    /// 2. Otherwise, if something is already listening on the bundled
    ///    port, adopt it too — covers manually-started daemons and
    ///    stale-pidfile edge cases so we never spawn a duplicate on the
    ///    same port.
    /// 3. Otherwise, spawn the daemon from `spec.binary`, write the
    ///    pidfile for the next launch, and poll the port until it's
    ///    reachable (bounded by [`PORT_READY_TIMEOUT`]).
    pub async fn start(spec: SpawnSpec) -> Result<Self, SupervisorError> {
        let SpawnSpec {
            binary,
            args,
            pidfile,
            port,
        } = spec;

        if let Some(pid) = read_live_pid(&pidfile) {
            return Ok(Self {
                pidfile,
                pid,
                mode: SupervisorMode::Adopted,
                owned_child: None,
                port,
            });
        }

        if port_is_listening(port).await {
            return Ok(Self {
                pidfile,
                pid: 0,
                mode: SupervisorMode::Adopted,
                owned_child: None,
                port,
            });
        }

        if let Some(parent) = pidfile.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut cmd = Command::new(&binary);
        cmd.args(&args);
        // Belt-and-braces for the post-spawn error paths below: if this
        // function returns `Err` between `spawn()` and the final `Ok`,
        // the child is dropped and tokio sends SIGKILL instead of
        // leaving an orphan ngIRCd on the supervised port. Without this
        // a `tokio::fs::write(&pidfile, …)` failure (e.g. ENOSPC, EIO,
        // readonly fs) would leak a running daemon with no pidfile,
        // and the next launch would reconcile via port probe only —
        // or spawn a duplicate if the port probe raced.
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let pid = child.id().ok_or_else(|| {
            SupervisorError::Io(std::io::Error::other(
                "spawned ngircd child did not expose a pid",
            ))
        })?;

        // Any `?` between here and the final `Ok` must not leak the
        // child. Capture every fallible step, kill on failure, then
        // propagate the original error so the caller sees the real
        // cause (not a shutdown error masking it).
        if let Err(e) = tokio::fs::write(&pidfile, format!("{pid}\n")).await {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(SupervisorError::Io(e));
        }

        let _ = wait_for_port(port, PORT_READY_TIMEOUT).await;

        Ok(Self {
            pidfile,
            pid,
            mode: SupervisorMode::Owned,
            owned_child: Some(child),
            port,
        })
    }

    /// Port the supervised daemon is listening on (as recorded in the
    /// spawn spec / workshop config).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// PID of the supervised daemon. Zero when the supervisor reconciled
    /// via the port probe without a readable pidfile — in that case the
    /// supervisor won't send signals on shutdown anyway.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Whether this supervisor owns the child process (spawned it) or
    /// merely reconciled to an existing daemon.
    pub fn owns_child(&self) -> bool {
        matches!(self.mode, SupervisorMode::Owned)
    }

    /// Graceful shutdown. Only takes action when we spawned the child;
    /// adopted supervisors return immediately so the daemon stays alive
    /// for the next launch to reconcile.
    ///
    /// Sequence for an owned child:
    /// 1. SIGTERM via `libc::kill`.
    /// 2. `child.wait()` with a [`SHUTDOWN_GRACE`] timeout.
    /// 3. On timeout, SIGKILL and a final unbounded `child.wait()`.
    /// 4. Remove the pidfile so the next launch spawns fresh.
    pub async fn shutdown(mut self) {
        let Some(mut child) = self.owned_child.take() else {
            return;
        };
        let pid = self.pid;

        send_signal(pid, Signal::Term);

        let graceful = tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await;
        if graceful.is_err() {
            send_signal(pid, Signal::Kill);
            let _ = child.wait().await;
        }

        let _ = tokio::fs::remove_file(&self.pidfile).await;
    }
}

/// Read `pidfile` and return the PID only if the process is still alive
/// (as reported by `kill(pid, 0)` on unix). A missing file, an
/// unparseable payload, or a dead PID all resolve to `None` so the
/// caller treats the reconcile path the same as "no prior daemon".
fn read_live_pid(pidfile: &Path) -> Option<u32> {
    let contents = std::fs::read_to_string(pidfile).ok()?;
    let pid: u32 = contents.trim().parse().ok()?;
    if pid == 0 {
        return None;
    }
    if pid_is_alive(pid) { Some(pid) } else { None }
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` only checks permission to signal, never
    // actually delivers one. Any unsigned-to-signed cast that overflows
    // produces a pid that almost certainly isn't ours, and the call
    // just returns ESRCH — still a safe no-op.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    // PR #50 Copilot c3: `kill(pid, 0)` returns -1 for both "process
    // does not exist" (ESRCH) and "process exists but we can't signal
    // it" (EPERM). Only ESRCH means the PID is dead; treating EPERM
    // as dead would misclassify a live daemon owned by another user
    // (or running under a different uid after a privilege drop) and
    // spawn a duplicate. Any other errno is conservatively treated
    // as "unknown → dead" because the reconcile path is cheap and a
    // false-negative just means one extra spawn attempt.
    let err = std::io::Error::last_os_error().raw_os_error();
    matches!(err, Some(libc::EPERM))
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    false
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: Signal) {
    if pid == 0 {
        return;
    }
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: plain libc call; failures (ESRCH after the process has
    // already exited) are ignored.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _signal: Signal) {}

/// Non-blocking TCP connect probe on `127.0.0.1:port`. Used to detect an
/// already-running daemon on the bundled port and to wait-for-ready
/// after spawn.
async fn port_is_listening(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

async fn wait_for_port(port: u16, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if port_is_listening(port).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn for_workshop_returns_none_without_bundled_port() {
        // Pre-`ryve init` workshop: no bundled port recorded, no daemon
        // to supervise. The helper bails before touching the filesystem.
        let tmp = TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        let cfg = WorkshopConfig::default();
        assert!(SpawnSpec::for_workshop(&ryve_dir, &cfg).is_none());
    }

    #[test]
    fn for_workshop_returns_none_without_config_file() {
        // Port recorded but `.ryve/ircd/ircd.conf` hasn't been written
        // yet (spark ryve-4d5881c2 hasn't run in this tree). The
        // supervisor must skip cleanly instead of spawning ngircd
        // against a missing config.
        let tmp = TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        let cfg = WorkshopConfig {
            irc_bundled_port: Some(6971),
            ..Default::default()
        };
        assert!(SpawnSpec::for_workshop(&ryve_dir, &cfg).is_none());
    }

    #[test]
    fn pidfile_path_is_under_ryve_ircd() {
        let tmp = TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        let p = ircd_pidfile_path(&ryve_dir);
        assert!(p.ends_with("ircd/ircd.pid"), "got: {p:?}");
        assert!(p.starts_with(tmp.path().join(".ryve")));
    }

    #[test]
    fn config_path_is_under_ryve_ircd() {
        let tmp = TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        let p = ircd_config_path(&ryve_dir);
        assert!(p.ends_with("ircd/ircd.conf"), "got: {p:?}");
    }

    #[test]
    fn read_live_pid_rejects_missing_file() {
        let tmp = TempDir::new().unwrap();
        assert!(read_live_pid(&tmp.path().join("missing.pid")).is_none());
    }

    #[test]
    fn read_live_pid_rejects_garbage() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("garbage.pid");
        std::fs::write(&p, "not-a-number").unwrap();
        assert!(read_live_pid(&p).is_none());
    }

    #[test]
    fn read_live_pid_rejects_dead_pid() {
        // PID 2^31 - 1 is well above any practical pid_max; kill(pid, 0)
        // returns ESRCH, so the supervisor treats the pidfile as stale
        // and the caller spawns fresh. This is the "crashed last
        // launch" reconcile path.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("stale.pid");
        std::fs::write(&p, "2147483646\n").unwrap();
        assert!(read_live_pid(&p).is_none());
    }

    #[test]
    fn read_live_pid_accepts_live_pid() {
        // Our own pid is definitely alive — round-trips through a
        // pidfile and gets resurrected as `Some`.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("live.pid");
        let me = std::process::id();
        std::fs::write(&p, format!("{me}\n")).unwrap();
        assert_eq!(read_live_pid(&p), Some(me));
    }
}

/// Integration-level tests for the supervisor. These spawn real child
/// processes (`/bin/sleep` as a stand-in daemon) so the spawn + restart-
/// reconcile path runs against real pidfiles, real PIDs, and real
/// SIGTERM/SIGKILL — the only thing faked is ngIRCd's port-binding
/// behaviour, which the supervisor treats as best-effort anyway. Spark
/// ryve-242252b0 [sp-31659bbb] acceptance: "Integration test covers the
/// spawn + restart-reconcile path using a TempDir workshop."
#[cfg(all(test, unix))]
mod integration {
    use std::net::TcpListener;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    /// Reserve and immediately release a TCP port so the supervisor's
    /// port-probe (which connect()s against 127.0.0.1:port) treats it as
    /// "nothing listening" and takes the spawn branch. The test fake
    /// daemon never binds, so using any port here is safe — we just
    /// need something the reconcile probe won't spuriously accept.
    fn reserve_free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    /// Build a spawn spec pointing at `/bin/sleep` as the stand-in
    /// daemon. `sleep 60` gives us a long-lived child whose PID we can
    /// read from the supervisor's own pidfile.
    fn sleep_spec(workshop: &TempDir, port: u16) -> SpawnSpec {
        let pidfile = workshop.path().join(".ryve/ircd/ircd.pid");
        SpawnSpec {
            binary: PathBuf::from("/bin/sleep"),
            args: vec!["60".into()],
            pidfile,
            port,
        }
    }

    /// Spawn path: a fresh TempDir workshop has no prior daemon, so
    /// `start()` forks the fake daemon, writes the pidfile, and owns the
    /// child. The shutdown then SIGTERMs the sleep child and removes
    /// the pidfile — the canonical happy path.
    #[tokio::test]
    async fn spawn_writes_pidfile_and_owns_child() {
        let workshop = TempDir::new().unwrap();
        let port = reserve_free_port();
        let spec = sleep_spec(&workshop, port);

        let sup = IrcdSupervisor::start(spec.clone())
            .await
            .expect("start succeeds on a fresh workshop");

        assert!(sup.owns_child(), "fresh spawn should own the child");
        assert_ne!(sup.pid(), 0, "owned supervisor must report a real pid");
        assert!(spec.pidfile.exists(), "pidfile must be written after spawn");

        let pidfile_contents = std::fs::read_to_string(&spec.pidfile).unwrap();
        let pid_on_disk: u32 = pidfile_contents.trim().parse().unwrap();
        assert_eq!(pid_on_disk, sup.pid());
        assert!(pid_is_alive(pid_on_disk), "child must be running");

        sup.shutdown().await;

        assert!(!pid_is_alive(pid_on_disk), "shutdown must reap the child");
        assert!(
            !spec.pidfile.exists(),
            "shutdown must remove the pidfile so the next launch spawns fresh"
        );
    }

    /// Restart-reconcile path: a prior launch left a live pidfile, so
    /// `start()` must adopt that daemon instead of spawning a duplicate.
    /// Acceptance: "reconciles the process on restart so a second launch
    /// does not produce a duplicate daemon."
    #[tokio::test]
    async fn second_start_reconciles_without_spawning_duplicate() {
        let workshop = TempDir::new().unwrap();
        let port = reserve_free_port();
        let spec = sleep_spec(&workshop, port);

        let first = IrcdSupervisor::start(spec.clone())
            .await
            .expect("first start succeeds");
        let first_pid = first.pid();
        assert!(first.owns_child());

        // Simulate a second Ryve launch: the prior process handle is
        // gone (as if the app had restarted) but the pidfile on disk
        // still points at a live daemon. The reconcile branch must
        // adopt it without a new spawn.
        let second = IrcdSupervisor::start(spec.clone())
            .await
            .expect("second start succeeds via reconcile");

        assert!(
            !second.owns_child(),
            "second supervisor must adopt, not spawn"
        );
        assert_eq!(
            second.pid(),
            first_pid,
            "reconciled supervisor must track the original pid"
        );
        assert!(pid_is_alive(first_pid), "original daemon still running");

        // Shutdown of the adopted supervisor is a no-op — it must NOT
        // kill the child we still own via `first`.
        second.shutdown().await;
        assert!(
            pid_is_alive(first_pid),
            "adopted shutdown must not touch the daemon"
        );

        // Now the owning supervisor cleans up for real.
        first.shutdown().await;
        assert!(!pid_is_alive(first_pid));
        assert!(!spec.pidfile.exists());
    }

    /// Stale pidfile edge case: a crashed previous launch left a pidfile
    /// pointing at a dead PID. The supervisor must treat the file as
    /// absent and spawn a fresh daemon, overwriting the stale entry so
    /// a third launch can reconcile cleanly.
    #[tokio::test]
    async fn stale_pidfile_is_replaced_by_fresh_spawn() {
        let workshop = TempDir::new().unwrap();
        let port = reserve_free_port();
        let spec = sleep_spec(&workshop, port);
        std::fs::create_dir_all(spec.pidfile.parent().unwrap()).unwrap();
        // PID just under i32::MAX — almost certainly dead, kill(pid, 0)
        // returns ESRCH so read_live_pid rejects it.
        std::fs::write(&spec.pidfile, "2147483646\n").unwrap();

        let sup = IrcdSupervisor::start(spec.clone())
            .await
            .expect("start succeeds despite stale pidfile");

        assert!(sup.owns_child(), "stale pidfile must not trigger reconcile");
        let on_disk: u32 = std::fs::read_to_string(&spec.pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            on_disk,
            sup.pid(),
            "fresh spawn must overwrite the stale pid"
        );

        sup.shutdown().await;
    }
}
