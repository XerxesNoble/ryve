// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streaming-heartbeat wrapper around a child process.
//!
//! Spark ryve-9660d5d2 [sp-9660d5d2]: long-running subcommands (notably
//! `cargo test`) can go minutes without writing to stdout. Claude's
//! coding-agent transport kills a Hand session after ~5 minutes of
//! stream silence ("Stream idle timeout - partial response received"),
//! which ends the Hand mid-task. This module wraps a
//! [`tokio::process::Command`] and injects a heartbeat line onto the
//! caller-supplied output sink whenever the child has been silent for
//! longer than a configured interval, while still forwarding the
//! child's real stdout/stderr byte-for-byte.
//!
//! ## Invariants
//!
//! - A heartbeat line is emitted at least once per configured interval
//!   while the child produces no output. The default interval is 30 s
//!   — safely below Claude's 5-minute idle threshold.
//! - Child stdout/stderr bytes are forwarded in the order they were
//!   produced. The wrapper never drops, reorders, or rewrites real
//!   output. Heartbeats are only injected during silence, so they can
//!   never split a continuous burst of child output.
//! - Once the child exits and its pipes reach EOF, no further
//!   heartbeats are emitted. The wrapper drains any remaining buffered
//!   output before returning.
//!
//! The sibling spark ryve-b7f7f1fa wires this wrapper into
//! `src/hand_spawn.rs`; until then the module is unreferenced from the
//! binary entry-point, mirroring the `sparks_filter` staging pattern.

use std::process::{ExitStatus, Stdio};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

/// Default heartbeat interval. 30 s leaves a comfortable margin under
/// Claude's ~5-minute stream-idle timeout while not spamming output.
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Default heartbeat payload. Distinctive enough for downstream
/// consumers (log scraping, humans) to recognise as non-child output.
pub const DEFAULT_HEARTBEAT_LINE: &str = "[stream-heartbeat] still running";

/// Outcome of a wrapped child run.
#[derive(Debug, Clone)]
pub struct StreamOutcome {
    /// Exit status of the child process.
    pub status: ExitStatus,
    /// Number of heartbeat lines emitted during the run. Exposed
    /// mainly so tests (and higher-level diagnostics) can assert the
    /// heartbeat timer fired as expected.
    pub heartbeats_emitted: u64,
}

/// Configuration for a wrapped child run. Construct with
/// [`StreamHeartbeat::new`], tune with the `with_*` builders, then
/// invoke with [`StreamHeartbeat::run`].
#[derive(Debug, Clone)]
pub struct StreamHeartbeat {
    interval: Duration,
    heartbeat_line: String,
}

impl Default for StreamHeartbeat {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamHeartbeat {
    pub fn new() -> Self {
        Self {
            interval: DEFAULT_HEARTBEAT_INTERVAL,
            heartbeat_line: DEFAULT_HEARTBEAT_LINE.to_string(),
        }
    }

    /// Override the silent-window after which a heartbeat fires.
    /// Callers must keep the interval well under Claude's ~5-minute
    /// stream-idle threshold. A zero interval is clamped to 1 ms to
    /// avoid a degenerate spin on the timer.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = if interval.is_zero() {
            Duration::from_millis(1)
        } else {
            interval
        };
        self
    }

    /// Override the heartbeat payload. The wrapper always terminates
    /// the line with `\n` so line-buffered readers flush it promptly.
    pub fn with_heartbeat_line(mut self, line: impl Into<String>) -> Self {
        self.heartbeat_line = line.into();
        self
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn heartbeat_line(&self) -> &str {
        &self.heartbeat_line
    }

    /// Spawn `command` with piped stdout+stderr, forward its output to
    /// `out`, and inject the heartbeat line whenever it has been idle
    /// for longer than the configured interval.
    ///
    /// The child's `stdout` and `stderr` are both multiplexed onto
    /// `out` in arrival order. The caller keeps ownership of any state
    /// behind `out` (so passing `&mut Vec<u8>` is a valid capture
    /// strategy for tests).
    pub async fn run<W>(&self, mut command: Command, out: W) -> std::io::Result<StreamOutcome>
    where
        W: AsyncWrite + Unpin + Send,
    {
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .expect("child stdout was piped but handle missing");
        let stderr = child
            .stderr
            .take()
            .expect("child stderr was piped but handle missing");

        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let tx_err = tx.clone();
        tokio::spawn(forward_reader(stdout, tx));
        tokio::spawn(forward_reader(stderr, tx_err));

        let heartbeat_bytes = {
            let mut v = self.heartbeat_line.clone().into_bytes();
            if !v.ends_with(b"\n") {
                v.push(b'\n');
            }
            v
        };

        let mut out = out;
        let mut heartbeats: u64 = 0;
        let mut child_status: Option<ExitStatus> = None;
        let mut last_output = Instant::now();
        let mut wait_fut = Box::pin(child.wait());

        loop {
            let deadline = last_output + self.interval;
            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            tokio::select! {
                biased;
                maybe_chunk = rx.recv() => {
                    match maybe_chunk {
                        Some(chunk) => {
                            out.write_all(&chunk).await?;
                            out.flush().await?;
                            last_output = Instant::now();
                        }
                        None => {
                            // Both forwarders have closed the channel,
                            // meaning stdout + stderr have hit EOF.
                            // Resolve the exit status (may already be
                            // captured) and finish.
                            let status = match child_status {
                                Some(s) => s,
                                None => (&mut wait_fut).await?,
                            };
                            return Ok(StreamOutcome {
                                status,
                                heartbeats_emitted: heartbeats,
                            });
                        }
                    }
                }
                _ = &mut sleep, if child_status.is_none() => {
                    out.write_all(&heartbeat_bytes).await?;
                    out.flush().await?;
                    heartbeats += 1;
                    last_output = Instant::now();
                }
                status = &mut wait_fut, if child_status.is_none() => {
                    child_status = Some(status?);
                    // Keep looping so any pending stdout/stderr bytes
                    // still drain through `rx.recv()` before we return.
                }
            }
        }
    }
}

async fn forward_reader<R>(mut reader: R, tx: mpsc::UnboundedSender<Vec<u8>>)
where
    R: AsyncRead + Unpin,
{
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration as StdDuration;

    use super::*;

    #[tokio::test]
    async fn silent_child_emits_heartbeats() {
        let sh = StreamHeartbeat::new()
            .with_interval(Duration::from_millis(100))
            .with_heartbeat_line("HB");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 0.5");

        let mut out = Vec::<u8>::new();
        let outcome = sh.run(cmd, &mut out).await.expect("run ok");

        assert!(outcome.status.success(), "sleep 0.5 should exit 0");
        assert!(
            outcome.heartbeats_emitted >= 2,
            "expected >=2 heartbeats across a 500 ms silent window with a 100 ms \
             interval, got {}",
            outcome.heartbeats_emitted,
        );

        let captured = String::from_utf8(out).expect("valid utf8");
        let observed = captured.matches("HB\n").count() as u64;
        assert_eq!(
            observed, outcome.heartbeats_emitted,
            "heartbeats in the captured stream must match the reported count; \
             captured={captured:?}",
        );
    }

    #[tokio::test]
    async fn noisy_child_suppresses_heartbeats_and_preserves_order() {
        let sh = StreamHeartbeat::new()
            .with_interval(Duration::from_millis(500))
            .with_heartbeat_line("HB");
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("for i in 1 2 3 4 5; do echo chunk$i; sleep 0.03; done");

        let mut out = Vec::<u8>::new();
        let outcome = sh.run(cmd, &mut out).await.expect("run ok");

        assert!(outcome.status.success(), "shell loop should exit 0");
        assert_eq!(
            outcome.heartbeats_emitted, 0,
            "child produced a chunk every ~30 ms for ~150 ms total — the 500 ms \
             heartbeat window should never elapse",
        );

        let captured = String::from_utf8(out).expect("valid utf8");
        assert!(
            !captured.contains("HB"),
            "no heartbeat line should appear in the forwarded stream; got {captured:?}",
        );
        for i in 1..=5 {
            let needle = format!("chunk{i}");
            assert!(
                captured.contains(&needle),
                "forwarded stream must contain {needle}; got {captured:?}",
            );
        }
        let pos_first = captured.find("chunk1").unwrap();
        let pos_last = captured.find("chunk5").unwrap();
        assert!(
            pos_first < pos_last,
            "child stdout order must be preserved; got {captured:?}",
        );
    }

    #[tokio::test]
    async fn clean_shutdown_returns_promptly_with_zero_heartbeats() {
        let sh = StreamHeartbeat::new()
            .with_interval(Duration::from_secs(10))
            .with_heartbeat_line("HB");
        let cmd = Command::new("true");

        let mut out = Vec::<u8>::new();
        let start = std::time::Instant::now();
        let outcome = sh.run(cmd, &mut out).await.expect("run ok");
        let elapsed = start.elapsed();

        assert!(outcome.status.success(), "/usr/bin/true should exit 0");
        assert_eq!(
            outcome.heartbeats_emitted, 0,
            "an instantly-exiting child must not trigger a heartbeat",
        );
        assert!(out.is_empty(), "/usr/bin/true produces no output");
        assert!(
            elapsed < StdDuration::from_secs(2),
            "wrapper must return promptly once the child exits, took {elapsed:?}",
        );
    }

    #[tokio::test]
    async fn non_zero_exit_is_reported() {
        let sh = StreamHeartbeat::new().with_interval(Duration::from_secs(10));
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("exit 7");

        let mut out = Vec::<u8>::new();
        let outcome = sh.run(cmd, &mut out).await.expect("run ok");

        assert_eq!(
            outcome.status.code(),
            Some(7),
            "wrapper must propagate the child's exit code unchanged",
        );
        assert_eq!(outcome.heartbeats_emitted, 0);
    }

    #[tokio::test]
    async fn final_child_output_is_not_swallowed() {
        // Reproduces the drain-ordering concern: the child writes, then
        // exits. `child.wait()` may resolve before the final pipe reads
        // complete. The wrapper must keep draining `rx` past the exit
        // status arm and flush the final bytes before returning.
        let sh = StreamHeartbeat::new()
            .with_interval(Duration::from_secs(10))
            .with_heartbeat_line("HB");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo final-line");

        let mut out = Vec::<u8>::new();
        let outcome = sh.run(cmd, &mut out).await.expect("run ok");

        assert!(outcome.status.success());
        assert_eq!(outcome.heartbeats_emitted, 0);
        let captured = String::from_utf8(out).expect("valid utf8");
        assert!(
            captured.contains("final-line"),
            "final child output must be forwarded, got {captured:?}",
        );
    }

    #[tokio::test]
    async fn stderr_is_multiplexed_onto_the_same_sink() {
        let sh = StreamHeartbeat::new()
            .with_interval(Duration::from_secs(10))
            .with_heartbeat_line("HB");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo err-line 1>&2");

        let mut out = Vec::<u8>::new();
        let outcome = sh.run(cmd, &mut out).await.expect("run ok");

        assert!(outcome.status.success());
        let captured = String::from_utf8(out).expect("valid utf8");
        assert!(
            captured.contains("err-line"),
            "stderr output must also reach the wrapper's sink, got {captured:?}",
        );
    }
}
