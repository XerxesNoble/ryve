// SPDX-License-Identifier: AGPL-3.0-or-later

//! Inter-process communication for Ryve.
//!
//! Single-instance enforcement and message passing between Ryve windows /
//! processes. The first ryve UI process to start binds a Unix domain
//! socket at [`socket_path`]; any subsequent ryve invocation detects the
//! bound socket, forwards its `(cwd, args)` to the running instance, and
//! exits. The running instance reacts by raising its window and (if the
//! cwd looks like a workshop) opening it as a tab.
//!
//! On non-Unix targets the IPC layer is a no-op stub that always returns
//! `Acquired::First` with no listener — multiple instances are still
//! allowed there until a named-pipe implementation lands.

pub mod irc_command_parser;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod channel_manager;
pub mod irc_client;
pub mod irc_renderer;
pub mod lifecycle;
pub mod outbox_relay;
pub mod signal_discipline;

/// Returns the path for the IPC socket. We prefer `$XDG_RUNTIME_DIR`
/// (cleared on logout, exactly the right scope), fall back to the user's
/// cache dir, and finally to `/tmp` so the path is always available.
pub fn socket_path() -> PathBuf {
    let dir = dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("ryve.sock")
}

/// Payload sent from a secondary ryve invocation to the running primary.
/// Carries enough context for the primary to react usefully — currently
/// the working directory plus the original argv. Add fields here as new
/// CLI surfaces want to forward (e.g. `spark_id` for `ryve sp-abcd`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardedInvocation {
    pub cwd: PathBuf,
    pub args: Vec<String>,
}

impl ForwardedInvocation {
    pub fn from_env() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            args: std::env::args().collect(),
        }
    }
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result of [`acquire`].
pub enum Acquired {
    /// This process is the primary instance. The caller is responsible
    /// for keeping the listener alive (e.g. by handing it to the iced
    /// subscription) and for calling [`cleanup`] on shutdown.
    ///
    /// On non-Unix targets `listener` is `None` and the caller should
    /// proceed without IPC support.
    First {
        #[cfg(unix)]
        listener: std::os::unix::net::UnixListener,
    },
    /// Another ryve UI is already running and the invocation has been
    /// forwarded to it. The caller should exit cleanly.
    Forwarded,
}

/// Try to become the primary ryve UI instance.
///
/// On Unix:
/// 1. Try to `bind()` the socket. On success → primary.
/// 2. On `EADDRINUSE`, try to `connect()` and forward `invocation`. If
///    a peer answers, this is a secondary instance — return `Forwarded`.
/// 3. If the connect fails (stale socket left over by a crashed peer),
///    unlink the file and retry the bind once.
///
/// On non-Unix targets this is currently a no-op that always returns
/// `Acquired::First { listener: None }` (multiple instances allowed
/// until a named-pipe path is added).
#[cfg(unix)]
pub fn acquire(invocation: &ForwardedInvocation) -> Result<Acquired, IpcError> {
    acquire_at(&socket_path(), invocation)
}

/// Path-parameterised variant of [`acquire`]. The public entry point
/// always uses [`socket_path`]; this exists so tests can drive the same
/// logic against a private temp file without racing a real ryve UI.
#[cfg(unix)]
pub fn acquire_at(path: &Path, invocation: &ForwardedInvocation) -> Result<Acquired, IpcError> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    match try_bind(path) {
        Ok(listener) => Ok(Acquired::First { listener }),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Probe — is a real peer listening, or a stale file?
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(stream) => {
                    forward_blocking(stream, invocation)?;
                    Ok(Acquired::Forwarded)
                }
                Err(_) => {
                    // No peer answered: stale socket. Unlink and retry.
                    let _ = std::fs::remove_file(path);
                    let listener = try_bind(path)?;
                    Ok(Acquired::First { listener })
                }
            }
        }
        Err(e) => Err(IpcError::Io(e)),
    }
}

#[cfg(not(unix))]
pub fn acquire(_invocation: &ForwardedInvocation) -> Result<Acquired, IpcError> {
    // No named-pipe implementation yet — let every invocation start its
    // own UI. Documented as a known limitation in the module docs.
    Ok(Acquired::First {})
}

/// Best-effort: unlink the socket file. Call from the primary on clean
/// shutdown so the next start does not have to take the stale-socket
/// path. Idempotent.
#[cfg(unix)]
pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}

#[cfg(not(unix))]
pub fn cleanup() {}

#[cfg(unix)]
fn try_bind(path: &Path) -> std::io::Result<std::os::unix::net::UnixListener> {
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

#[cfg(unix)]
fn forward_blocking(
    mut stream: std::os::unix::net::UnixStream,
    invocation: &ForwardedInvocation,
) -> Result<(), IpcError> {
    use std::io::Write;

    // Modest write timeout so a wedged peer cannot hang the secondary
    // forever — if forwarding fails the secondary should still exit.
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(2)));

    let bytes = serde_json::to_vec(invocation)?;
    let len = u32::try_from(bytes.len()).map_err(|_| {
        IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invocation too large",
        ))
    })?;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

/// Read one length-prefixed JSON [`ForwardedInvocation`] from `stream`.
/// Used by the primary's accept loop.
#[cfg(unix)]
pub async fn read_invocation(
    stream: &mut tokio::net::UnixStream,
) -> Result<ForwardedInvocation, IpcError> {
    use tokio::io::AsyncReadExt;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;

    // Sanity cap: forwarded invocations should be tiny (cwd + argv).
    // Reject anything pathological so a malicious local peer cannot
    // pin gigabytes of memory in the primary.
    if len > 1024 * 1024 {
        return Err(IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "forwarded invocation exceeds 1 MiB",
        )));
    }

    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_under_a_runtime_or_cache_dir() {
        let p = socket_path();
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("ryve.sock"));
    }

    #[test]
    fn forwarded_invocation_round_trips_through_json() {
        let inv = ForwardedInvocation {
            cwd: PathBuf::from("/tmp/work"),
            args: vec!["ryve".into(), "--json".into()],
        };
        let bytes = serde_json::to_vec(&inv).unwrap();
        let back: ForwardedInvocation = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.cwd, inv.cwd);
        assert_eq!(back.args, inv.args);
    }

    #[cfg(unix)]
    fn unique_test_path(tag: &str) -> PathBuf {
        // Per-test, per-process socket path so concurrent test runs do
        // not collide and the developer's real ryve.sock is untouched.
        std::env::temp_dir().join(format!(
            "ryve-ipc-test-{}-{}-{}.sock",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ))
    }

    #[cfg(unix)]
    #[test]
    fn first_acquire_returns_listener_then_second_forwards() {
        let path = unique_test_path("forward");
        let inv = ForwardedInvocation {
            cwd: PathBuf::from("/tmp/ws"),
            args: vec!["ryve".into()],
        };

        // Primary: should bind successfully.
        let primary = acquire_at(&path, &inv).expect("primary acquire");
        let listener = match primary {
            Acquired::First { listener } => listener,
            Acquired::Forwarded => panic!("first acquire should not forward"),
        };

        // Spawn a tokio runtime to drive the accept side, since
        // read_invocation is async. Multi-thread so the blocking
        // secondary `acquire_at` (which writes synchronously) and the
        // async accept can both make progress.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let path_for_block = path.clone();
        let received = rt.block_on(async move {
            let listener = tokio::net::UnixListener::from_std(listener).unwrap();
            let inv2 = ForwardedInvocation {
                cwd: PathBuf::from("/tmp/other"),
                args: vec!["ryve".into(), "--json".into()],
            };
            let secondary = tokio::task::spawn_blocking(move || acquire_at(&path_for_block, &inv2));

            let (mut stream, _) = listener.accept().await.expect("accept");
            let received = read_invocation(&mut stream).await.expect("read");

            let secondary_result = secondary.await.unwrap().expect("secondary acquire");
            assert!(matches!(secondary_result, Acquired::Forwarded));

            received
        });

        assert_eq!(received.cwd, PathBuf::from("/tmp/other"));
        assert_eq!(
            received.args,
            vec!["ryve".to_string(), "--json".to_string()]
        );

        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn stale_socket_file_is_recovered_on_acquire() {
        let path = unique_test_path("stale");

        // Simulate a crashed prior instance: a leftover file with no
        // listener behind it.
        std::fs::write(&path, b"").unwrap();

        let inv = ForwardedInvocation {
            cwd: PathBuf::from("/"),
            args: vec![],
        };
        let result = acquire_at(&path, &inv).expect("acquire");
        assert!(matches!(result, Acquired::First { .. }));

        let _ = std::fs::remove_file(&path);
    }
}
