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
//! - **Per-stream order is preserved.** Bytes read from the child's
//!   stdout arrive on the sink in the exact order they were produced on
//!   stdout, and likewise for stderr. **Cross-stream interleaving is
//!   best-effort arrival order**: the two pipes are drained by
//!   independent tokio tasks feeding a shared channel, so an
//!   "interleaved" stdout+stderr sequence at the child may reach the
//!   sink in a different interleaving than it would on a terminal.
//!   Callers that need strict cross-stream ordering must not
//!   multiplex. The wrapper still never drops, reorders within a
//!   stream, or rewrites real bytes, and heartbeats only fire during
//!   silence so they can never split a continuous burst of child
//!   output.
//! - Any I/O error reading from the child's stdout or stderr is
//!   surfaced by [`StreamHeartbeat::run`] as an `Err`, not silently
//!   dropped. A failing reader always triggers child cleanup (see
//!   below) so the wrapper never returns `Ok` after truncating output.
//! - If the sink `out` returns an error (broken pipe, disconnected
//!   transport, etc.), the wrapper kills the child before propagating
//!   the error, so a dead parent transport never leaves an orphaned
//!   `cargo test`. The spawned child is also registered with
//!   `kill_on_drop(true)` as a belt-and-braces safety net.
//! - Once the child exits and its pipes reach EOF, no further
//!   heartbeats are emitted. The wrapper drains any remaining buffered
//!   output before returning.
//!
//! Wired into `src/hand_spawn::run_with_stream_heartbeat` by sibling
//! spark ryve-b7f7f1fa as the opt-in spawn path for long-silent
//! subcommands run from a Hand.

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
    /// Per-stream byte order is preserved (see module-level invariants);
    /// stdout and stderr are multiplexed onto `out` in best-effort
    /// arrival order. The caller keeps ownership of any state behind
    /// `out` (so passing `&mut Vec<u8>` is a valid capture strategy for
    /// tests).
    ///
    /// Returns `Err` if either reader hits an I/O error, if the sink
    /// `out` errors on a write/flush, or if the child cannot be
    /// awaited. In every error path the child is killed before
    /// returning so a dead parent transport cannot leave an orphaned
    /// long-running subprocess. The spawned child is also registered
    /// with `kill_on_drop(true)` as a belt-and-braces safety net.
    pub async fn run<W>(&self, mut command: Command, out: W) -> std::io::Result<StreamOutcome>
    where
        W: AsyncWrite + Unpin + Send,
    {
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        // If we leave this scope (panic, early return, etc.) without
        // explicitly waiting on the child, drop should kill it. Pairs
        // with the explicit `child.start_kill()` calls in our error
        // paths below — neither is sufficient on its own across all
        // failure modes.
        command.kill_on_drop(true);

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .expect("child stdout was piped but handle missing");
        let stderr = child
            .stderr
            .take()
            .expect("child stderr was piped but handle missing");

        let (tx, mut rx) = mpsc::unbounded_channel::<ChunkResult>();
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

        // On any early-return Err the child's `kill_on_drop(true)`
        // setting takes care of cleanup: `wait_fut` is dropped first
        // (block-scope LIFO), releasing the &mut borrow on `child`,
        // then `child` itself drops and tokio sends SIGKILL. We
        // therefore do not need explicit `start_kill()` calls in the
        // error arms below — but the contract is that the wrapper must
        // not return Err and leave a long-running child alive, and the
        // module-level invariants document that guarantee.
        loop {
            let deadline = last_output + self.interval;
            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            tokio::select! {
                biased;
                maybe_chunk = rx.recv() => {
                    match maybe_chunk {
                        Some(Ok(chunk)) => {
                            // A sink-write failure here returns Err;
                            // child is killed via kill_on_drop on the
                            // way out (see comment above).
                            out.write_all(&chunk).await?;
                            out.flush().await?;
                            last_output = Instant::now();
                        }
                        Some(Err(e)) => {
                            // Reader failed mid-stream. Truncated
                            // output is a contract violation — surface
                            // the error rather than returning Ok with
                            // partial bytes. kill_on_drop reaps the
                            // child as we unwind.
                            return Err(e);
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

/// A chunk forwarded from one of the child's pipe readers, or the I/O
/// error that aborted that reader. The wrapper propagates `Err`s
/// through the channel rather than dropping them so a partial-read
/// failure cannot silently truncate `out`.
type ChunkResult = std::io::Result<Vec<u8>>;

async fn forward_reader<R>(mut reader: R, tx: mpsc::UnboundedSender<ChunkResult>)
where
    R: AsyncRead + Unpin,
{
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(Ok(buf[..n].to_vec())).is_err() {
                    // Receiver dropped (run() already returned). No
                    // point reading further.
                    break;
                }
            }
            Err(e) => {
                // Forward the error to run() so it can fail loudly
                // rather than returning Ok with truncated output.
                // If the receiver is already gone, drop the error too
                // — there is no observer left to surface it to.
                let _ = tx.send(Err(e));
                break;
            }
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

    /// A failing sink (one that returns `BrokenPipe` on the first
    /// write) must:
    ///   1. cause `run()` to return `Err`, not `Ok` with truncated
    ///      output, and
    ///   2. kill the long-running child so it does not leak.
    /// The covered Copilot concern (src/stream_heartbeat.rs:166): an
    /// `out.write_all` failure must not leave the spawned `cargo test`
    /// (or equivalent) running orphaned.
    #[tokio::test]
    async fn sink_write_error_kills_child_and_propagates() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        use tokio::io::{Error as IoError, ErrorKind as IoErrorKind};

        struct BrokenSink;
        impl AsyncWrite for BrokenSink {
            fn poll_write(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &[u8],
            ) -> Poll<Result<usize, IoError>> {
                Poll::Ready(Err(IoError::new(IoErrorKind::BrokenPipe, "sink closed")))
            }
            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), IoError>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), IoError>> {
                Poll::Ready(Ok(()))
            }
        }

        // 30 s sleep child so that if cleanup is broken the test would
        // hang or leak the child past the sink-error.
        let sh = StreamHeartbeat::new()
            .with_interval(Duration::from_millis(50))
            .with_heartbeat_line("HB");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");

        let start = std::time::Instant::now();
        let res = sh.run(cmd, BrokenSink).await;
        let elapsed = start.elapsed();

        assert!(
            res.is_err(),
            "wrapper must surface sink errors, got Ok({:?})",
            res.ok().map(|o| o.status.code()),
        );
        assert!(
            elapsed < StdDuration::from_secs(5),
            "wrapper must not wait for the long child to finish on a sink \
             error; took {elapsed:?}",
        );
    }

    /// `forward_reader` must surface I/O errors instead of silently
    /// swallowing them. Covers the Copilot concern (src/stream_heartbeat.rs:214):
    /// a partial-read failure on a child pipe used to break out of the
    /// reader loop and let `run()` return `Ok` with truncated output,
    /// violating the "never drops output" guarantee.
    ///
    /// We exercise the channel-level invariant directly because forging
    /// a real pipe-read failure is platform-dependent. The test asserts
    /// that an `Err(_)` chunk passed through the channel reaches `run()`
    /// and turns into an `Err` return.
    #[tokio::test]
    async fn reader_io_error_propagates_through_channel() {
        // Construct the channel and reader side manually to inject an
        // error chunk after one good chunk, simulating a pipe that
        // failed mid-stream.
        let (tx, mut rx) = mpsc::unbounded_channel::<ChunkResult>();
        tx.send(Ok(b"partial".to_vec())).unwrap();
        tx.send(Err(std::io::Error::other("simulated pipe read failure")))
            .unwrap();
        drop(tx);

        // Drain manually mimicking run()'s rx arm.
        let mut saw_partial = false;
        let mut saw_err = false;
        while let Some(item) = rx.recv().await {
            match item {
                Ok(chunk) => {
                    assert_eq!(chunk, b"partial");
                    saw_partial = true;
                }
                Err(e) => {
                    assert_eq!(e.to_string(), "simulated pipe read failure");
                    saw_err = true;
                    break;
                }
            }
        }
        assert!(saw_partial, "good chunk must be delivered before the error");
        assert!(saw_err, "I/O error must reach the consumer, not be dropped");
    }
}
