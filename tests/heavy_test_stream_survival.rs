// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Integration test for spark ryve-9350365a.
//
// A Hand that runs a long silent subprocess (notably the full-workspace
// `cargo test`) used to die with "Stream idle timeout - partial response
// received" after ~5 minutes of stdout silence. Sibling spark
// ryve-b7f7f1fa mitigated that by wrapping long-silent Hand commands with
// `run_with_stream_heartbeat`, which forwards the child's stdout and
// stderr byte-for-byte while injecting a heartbeat line whenever the
// child has been silent for longer than a configurable interval. The
// wrapper is reachable end-to-end via the `ryve hand exec-heartbeat`
// CLI subcommand that a Hand invokes in place of the bare command.
//
// This test drives that CLI end-to-end against a real silent subprocess
// and asserts (per Copilot review on PR #44):
//
//   1. **Heartbeats arrive on the parent's stdout *during* the silent
//      window**, not just after the child exits. We `spawn()` the
//      `ryve` binary and consume its stdout incrementally on a
//      background task, recording the wall-clock instant at which each
//      heartbeat line is observed. We then assert at least one
//      heartbeat arrived strictly before the child's silent sleep is
//      due to finish — a wrapper that buffered all heartbeats and
//      flushed them only on child exit would fail this check.
//   2. The wrapper still emits multiple heartbeats across the silent
//      window (the production-relevant liveness guarantee).
//   3. Every heartbeat is newline-terminated on the parent stream so a
//      line-buffered consumer (the streaming transport that motivated
//      this work) sees fresh traffic at each interval.
//   4. The wrapper returns the child's exit code unchanged so a real
//      `cargo test` failure would still propagate.
//   5. The wrapper finishes promptly once the child exits.
//
// Invariant from the spark: the test runs in reasonable CI time by
// shortening the heartbeat interval via the CLI flag rather than
// running a literal 10-minute sleep. We use `--interval-secs 1`
// against a 4 s silent child — the child outlives 4x the interval,
// which scales to a ~120 s silent window under the production 30 s
// default. That is well past any reasonable idle-window floor while
// finishing in ~4 s on CI.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Heartbeat line the wrapper injects on silence. Duplicated here so
/// this test stays a black-box CLI contract — the `ryve` binary is a
/// separate crate, so we cannot import
/// `stream_heartbeat::DEFAULT_HEARTBEAT_LINE` directly. If the
/// wrapper's default payload changes, this constant must move with it.
const HEARTBEAT_LINE: &str = "[stream-heartbeat] still running";

/// One observation of the child's stdout: the line content (with the
/// trailing newline stripped) and the wall-clock instant the parent
/// finished reading it. Recorded by a background reader thread so the
/// main test body can assert on *when* heartbeats arrived, not just
/// what the final stdout buffer contains.
#[derive(Debug, Clone)]
struct StdoutLine {
    text: String,
    seen_at: Instant,
}

#[test]
fn hand_survives_long_silent_subprocess_past_idle_threshold() {
    // 1 s heartbeat interval + 4 s silent child = the child outlives
    // 4x the interval, mirroring the ratio a production 30 s interval
    // has against a >2-minute silent window. Shortened-via-config per
    // the spark's CI-time invariant.
    let interval_secs: u64 = 1;
    let child_silent_secs: u64 = 4;

    let test_started = Instant::now();
    let mut child = Command::new(ryve_bin())
        .args([
            "hand",
            "exec-heartbeat",
            "--interval-secs",
            &interval_secs.to_string(),
            "--",
            "sh",
            "-c",
            &format!("sleep {child_silent_secs}"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `ryve hand exec-heartbeat`");

    // Drain stdout incrementally on a background thread so we can
    // distinguish "heartbeat arrived during silent window" from
    // "heartbeat was buffered until child exit". A wrapper that buffers
    // all output and flushes only on exit would still satisfy a
    // post-hoc count assertion against `Command::output()`; this
    // streaming reader is what makes the liveness check meaningful.
    let stdout = child.stdout.take().expect("piped stdout");
    let lines = Arc::new(Mutex::new(Vec::<StdoutLine>::new()));
    let lines_for_thread = Arc::clone(&lines);
    let stdout_thread = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(text) = line else { break };
            let stamped = StdoutLine {
                text,
                seen_at: Instant::now(),
            };
            lines_for_thread.lock().unwrap().push(stamped);
        }
    });

    // Drain stderr in parallel so a failing wrapper cannot block on a
    // full stderr pipe.
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_buf_thread = Arc::clone(&stderr_buf);
    let stderr_thread = thread::spawn(move || {
        use std::io::Read;
        let mut reader = stderr;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    stderr_buf_thread
                        .lock()
                        .unwrap()
                        .extend_from_slice(&buf[..n]);
                }
            }
        }
    });

    let status = child.wait().expect("wait for child");
    let elapsed = test_started.elapsed();
    stdout_thread.join().expect("stdout reader thread");
    stderr_thread.join().expect("stderr reader thread");

    let captured_lines = lines.lock().unwrap().clone();
    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes).into_owned();
    let captured_stdout: String = captured_lines
        .iter()
        .map(|l| format!("{}\n", l.text))
        .collect();

    // Acceptance 4: exit code propagated unchanged.
    assert!(
        status.success(),
        "wrapper must return the child's exit 0 unchanged; status={:?}, \
         stdout={captured_stdout:?}, stderr={stderr_str:?}",
        status.code(),
    );

    // Filter heartbeat observations.
    let heartbeats: Vec<&StdoutLine> = captured_lines
        .iter()
        .filter(|l| l.text == HEARTBEAT_LINE)
        .collect();

    // Acceptance 2: multiple heartbeats across the silent window.
    // Across a 4 s silent window with a 1 s interval the wrapper must
    // emit at least 3 heartbeat lines (the exact count depends on
    // timer quantisation on the runner; 3 is the safe lower bound
    // that still proves multiple intervals elapsed with the parent
    // stream kept alive).
    assert!(
        heartbeats.len() >= 3,
        "expected >=3 heartbeat lines across a {child_silent_secs}s silent window \
         with a {interval_secs}s interval, got {}; stdout={captured_stdout:?}",
        heartbeats.len(),
    );

    // Acceptance 1 (the core Copilot fix): at least one heartbeat must
    // have *arrived on the parent's stdout* while the child was still
    // sleeping. We compute the deadline as `child_silent_secs - 1` —
    // i.e. one full second before the child's sleep is due to finish.
    // Any heartbeat seen at or before that deadline arrived while the
    // child was still silent, so it cannot have been a post-exit
    // flush. A buffered wrapper that held all heartbeats until child
    // exit would emit them at >= T0+child_silent_secs and fail this
    // check. The 1 s margin absorbs process-startup latency and reader
    // thread scheduling jitter on slow CI runners.
    let live_deadline_secs = child_silent_secs.saturating_sub(1);
    let live_deadline = test_started + Duration::from_secs(live_deadline_secs);
    let live_heartbeats: Vec<&StdoutLine> = heartbeats
        .iter()
        .copied()
        .filter(|hb| hb.seen_at <= live_deadline)
        .collect();
    assert!(
        !live_heartbeats.is_empty(),
        "at least one heartbeat must arrive on the parent stream before the \
         child exits; first heartbeat was seen at {:?} (test started at T0, \
         live deadline was T0+{}s, child sleep was {}s); a wrapper that \
         buffered heartbeats and flushed them only at child exit would fail \
         this check. heartbeat_count={}, stdout={captured_stdout:?}",
        heartbeats
            .first()
            .map(|hb| hb.seen_at.duration_since(test_started)),
        live_deadline_secs,
        child_silent_secs,
        heartbeats.len(),
    );

    // Acceptance 3: every heartbeat arrived newline-terminated. Our
    // BufReader::lines() iterator strips the `\n`, so the very fact
    // that we observed a heartbeat as a discrete line proves it was
    // newline-terminated on the wire. Cross-check against the
    // newline-joined reconstruction to catch a wrapper that emitted
    // the heartbeat as part of a longer line.
    let newline_terminated = captured_stdout
        .matches(&format!("{HEARTBEAT_LINE}\n"))
        .count();
    assert_eq!(
        newline_terminated,
        heartbeats.len(),
        "every heartbeat must arrive newline-terminated on the parent stream; \
         terminated={newline_terminated}, total={}, stdout={captured_stdout:?}",
        heartbeats.len(),
    );

    // Acceptance 5: prompt return after child exit. An upper bound of
    // child_silent + 3 s catches both runaway loops and a wrapper
    // that keeps emitting heartbeats after the child is gone.
    let budget = Duration::from_secs(child_silent_secs + 3);
    assert!(
        elapsed < budget,
        "wrapper must return promptly after child exit; took {elapsed:?} \
         against a budget of {budget:?}",
    );
}
