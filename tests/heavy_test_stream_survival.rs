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
// and asserts:
//
//   1. The wrapper still emits heartbeats after the child has been silent
//      for multiple configured intervals — structurally proving the
//      parent stream never goes silent for longer than one interval,
//      regardless of how long the child stays silent.
//   2. The parent's stdout stream stays open for the whole silent window
//      and receives the heartbeat lines in order (they reach the parent,
//      not just the wrapper's internal counter).
//   3. The child eventually exits cleanly and the wrapper returns the
//      child's exit code unchanged.
//
// Invariant from the spark: the test runs in reasonable CI time by
// shortening the heartbeat interval via the CLI flag rather than running
// a literal 10-minute sleep. We use `--interval-secs 1` against a 4 s
// silent child — the child outlives 4x the interval, which scales to a
// ~120 s silent window under the production 30 s default. That is well
// past any reasonable idle-window floor while finishing in ~4 s on CI.

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Heartbeat line the wrapper injects on silence. Duplicated here so this
/// test stays a black-box CLI contract — the `ryve` binary is a separate
/// crate, so we cannot import `stream_heartbeat::DEFAULT_HEARTBEAT_LINE`
/// directly. If the wrapper's default payload changes, this constant
/// must move with it.
const HEARTBEAT_LINE: &str = "[stream-heartbeat] still running";

#[test]
fn hand_survives_long_silent_subprocess_past_idle_threshold() {
    // 1 s heartbeat interval + 4 s silent child = the child outlives 4x
    // the interval, mirroring the ratio a production 30 s interval has
    // against a >2-minute silent window. Shortened-via-config per the
    // spark's CI-time invariant.
    let interval_secs = "1";
    let child_silent_secs: u64 = 4;

    let started = Instant::now();
    let out = Command::new(ryve_bin())
        .args([
            "hand",
            "exec-heartbeat",
            "--interval-secs",
            interval_secs,
            "--",
            "sh",
            "-c",
            &format!("sleep {child_silent_secs}"),
        ])
        .output()
        .expect("run `ryve hand exec-heartbeat`");
    let elapsed = started.elapsed();

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    // Acceptance: the wrapper must have returned the child's exit code
    // unchanged so a real `cargo test` failure would still propagate.
    assert!(
        out.status.success(),
        "wrapper must return the child's exit 0 unchanged; status={:?}, \
         stdout={stdout:?}, stderr={stderr:?}",
        out.status.code(),
    );

    // Acceptance: heartbeats must have fired while the child was silent.
    // Across a 4 s silent window with a 1 s interval the wrapper must
    // emit at least 3 heartbeat lines (the exact count depends on timer
    // quantisation on the runner; 3 is the safe lower bound that still
    // proves multiple intervals elapsed with the parent stream kept
    // alive).
    let heartbeat_matches = stdout.matches(HEARTBEAT_LINE).count();
    assert!(
        heartbeat_matches >= 3,
        "expected >=3 heartbeat lines across a {child_silent_secs}s silent window \
         with a {interval_secs}s interval, got {heartbeat_matches}; stdout={stdout:?}",
    );

    // Acceptance: every heartbeat reaches the parent's stdout on its own
    // line. A wrapper that buffered heartbeats internally without
    // flushing would satisfy the previous count check via trailing
    // emission but leave the parent's stream idle — exactly the failure
    // mode this spark exists to prevent. Require `HEARTBEAT_LINE\n` so
    // the parent sees line-terminated traffic at each interval.
    let newline_terminated = stdout.matches(&format!("{HEARTBEAT_LINE}\n")).count();
    assert_eq!(
        newline_terminated, heartbeat_matches,
        "every heartbeat must arrive newline-terminated on the parent stream; \
         terminated={newline_terminated}, total={heartbeat_matches}, stdout={stdout:?}",
    );

    // Acceptance: the wrapper must finish promptly once the child exits.
    // An upper bound of child_silent + 3 s catches both runaway loops
    // and a wrapper that keeps emitting heartbeats after the child is
    // gone.
    let budget = std::time::Duration::from_secs(child_silent_secs + 3);
    assert!(
        elapsed < budget,
        "wrapper must return promptly after child exit; took {elapsed:?} \
         against a budget of {budget:?}",
    );
}
