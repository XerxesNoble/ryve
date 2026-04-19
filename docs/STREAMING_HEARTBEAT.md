# Streaming-Heartbeat Mitigation Pattern

This document is the written pattern for any Ryve subcommand (or
subprocess-launching helper) that shells out to a long-running child
process. Future heavy subcommands must adopt it so Hand sessions do not
die mid-task, and reviewers must hold new heavy-command additions to
this standard.

Parent epic: `ryve-32e95c28` — *Heavy-test stream-timeout fix:
streaming heartbeat during long cargo test invocations so Hand sessions
do not die mid-test.* Wrapper module landed in sibling spark
`ryve-9660d5d2`. (Spark IDs are workgraph-local; query them with
`ryve spark show <id>`.)

## 1. The stream-idle-timeout failure mode

A Hand session is a coding-agent subprocess (Claude Code / Codex /
OpenCode) talking to its vendor over HTTP streaming. The transport will
abort a streamed response that produces **no bytes for roughly five
minutes** — Claude surfaces this as

> Stream idle timeout - partial response received

and closes the session. A Hand that was mid-edit loses its context and
exits. The remedy must come from inside the Hand's own shell, not from
the Ryve orchestrator: Ryve does not see the stream.

The trap is triggered by any subcommand that stays silent longer than
~5 min. The empirical list so far:

- `cargo test` over the full workspace — the dominant case. Two Hands
  died on `ryve-f8e9931c` (the `hand_spawn` `#[serial]` work) because
  the heavy `hand_spawn` test suite takes well over five minutes
  without printing anything.
- Any `cargo build --release` on a cold target directory.
- Long `sqlx migrate` runs against non-trivial databases.
- Any compound command whose slowest stage prints nothing.

The failure mode is *silent and unrelated to exit code*: the child
would eventually succeed, but it never gets the chance because the
vendor has already hung up on the Hand that spawned it.

## 2. The `StreamHeartbeat` wrapper — API

The mitigation is a tokio wrapper that spawns a child process,
forwards its stdout/stderr byte-for-byte onto a caller-supplied sink,
and **injects a heartbeat line whenever the child has been silent for
longer than a configured interval**. Source:
[`src/stream_heartbeat.rs`](../src/stream_heartbeat.rs).

### Construction

```rust
use tokio::process::Command;
use crate::stream_heartbeat::StreamHeartbeat;

let sh = StreamHeartbeat::new();                                // defaults
let sh = StreamHeartbeat::new()
    .with_interval(std::time::Duration::from_secs(30))          // tune window
    .with_heartbeat_line("[cargo-test] still running");          // tune payload
```

Defaults:

| Field            | Default                         | Rationale |
|------------------|---------------------------------|-----------|
| `interval`       | `DEFAULT_HEARTBEAT_INTERVAL` — 30 s | Safely below the vendor's ~5 min idle threshold, not spammy. |
| `heartbeat_line` | `"[stream-heartbeat] still running"` | Distinctive enough for log scraping + humans to recognise as non-child output. |

### Running

```rust
let mut out = tokio::io::stdout();
let mut cmd = Command::new("cargo");
cmd.arg("test").arg("--workspace");

let outcome = sh.run(cmd, &mut out).await?;
assert!(outcome.status.success());
// outcome.heartbeats_emitted  — number of heartbeat lines injected
```

`run` takes ownership of the `Command`, forces `stdout(Stdio::piped())`
and `stderr(Stdio::piped())`, and multiplexes both onto the
caller-supplied `AsyncWrite` sink in arrival order.

### Invariants the wrapper guarantees

1. **Liveness.** A heartbeat line is emitted at least once per
   configured interval while the child produces no output. The timer
   resets every time the child writes, so a chatty child never
   triggers a heartbeat.
2. **Fidelity.** Child stdout and stderr bytes are forwarded in the
   order they were produced. The wrapper never drops, reorders, or
   rewrites real output. Heartbeats only fire during silence, so they
   cannot split a continuous burst.
3. **Clean shutdown.** Once the child exits and its pipes reach EOF,
   no further heartbeats are emitted. The wrapper drains any remaining
   buffered output before returning and propagates the child's exit
   status unchanged in `StreamOutcome::status`.
4. **Degenerate-interval safety.** A zero interval is clamped to 1 ms
   so the timer never spins.

Unit tests in `src/stream_heartbeat.rs` cover all four: silent child,
noisy child (order-preserving, zero heartbeats), clean immediate exit,
non-zero exit propagation, final-flush drain, and stderr multiplexing.

## 3. When a subcommand MUST use it

Use `StreamHeartbeat` for any subprocess invocation that satisfies
**both** of the following:

- it runs inside a Hand (or any coding-agent subprocess whose stdout
  flows over a streamed transport), and
- its 95th-percentile runtime is **≥ 60 s**, or a plausible input
  makes it silent for ≥ 60 s.

60 s is the threshold, not 5 min. The vendor's 5 min budget is the
ceiling; a 60 s floor leaves headroom for debuggers, slow disks, and
CI variance, and it matches the wrapper's default interval.

Do **not** wrap:

- Commands that are always fast (< 1 s typical), e.g. `git rev-parse`,
  `tmux display-message`. The heartbeat overhead is wasted and the
  heartbeat line would appear in logs that expect clean stdout.
- Commands whose output is being parsed programmatically, unless the
  parser is taught to skip the heartbeat line. The wrapper deliberately
  emits the heartbeat onto the same sink as child output; downstream
  parsers will see it.
- Fire-and-forget tmux launches like `tmux new-session -d`. The parent
  process returns immediately; the long-running work is in the detached
  session, which is out of scope for this wrapper.

### Reviewer checklist for new heavy subcommands

When a PR adds a new subcommand that shells out:

- [ ] Is the child's 95th-percentile runtime ≥ 60 s, or can it be
      silent for ≥ 60 s on plausible input?
- [ ] If yes, is it wrapped in `StreamHeartbeat`?
- [ ] Is the chosen interval ≤ 60 s? (Default 30 s is almost always
      correct.)
- [ ] Does the caller's output consumer tolerate a heartbeat line in
      the stream, or is there a parser that must be taught to skip it?

A "no" on any row is a blocking review comment.

## 4. Tuning the heartbeat interval

The default 30 s is correct for almost every caller. Tune only when
one of the following applies:

- **Noisy transport.** If the vendor's idle threshold is known to be
  shorter than Claude's ~5 min (e.g. a corporate proxy with a 60 s
  idle hangup), drop the interval so at least two heartbeats fit in
  the window. Rule of thumb: `interval ≤ transport_idle_threshold / 2`.
- **Log-volume pressure.** If the wrapper's output is being persisted
  to an archive sized in GB and the child runs for hours,
  `interval = Duration::from_secs(120)` cuts heartbeat lines 4×
  without getting close to the 5 min ceiling.
- **Unit tests.** Inside tests, a small interval
  (`Duration::from_millis(100)`) makes the silent-window path exercise
  in milliseconds instead of seconds. See
  `src/stream_heartbeat.rs::tests::silent_child_emits_heartbeats`.

Never raise the interval above **240 s** in production code. That
leaves less than one heartbeat per 5 min window and defeats the
mitigation on the first jittered transport the caller encounters.

`with_interval(Duration::ZERO)` is silently clamped to 1 ms — do not
rely on it. If a caller wants a heartbeat on every tick, pick an
explicit small duration.

## 5. Call-sites

Current and planned uses inside this crate:

- [`src/stream_heartbeat.rs`](../src/stream_heartbeat.rs) — the
  wrapper itself. Start here for the public API and the invariant
  comments.
- [`src/hand_spawn.rs`](../src/hand_spawn.rs) — Hand spawn path. The
  wrapper is wired in by sibling spark `ryve-b7f7f1fa` (*Wire
  stream-heartbeat into Hand subprocess spawn*). The relevant
  call-sites are the subprocess launches around
  [`launch_in_tmux`](../src/hand_spawn.rs) and the coding-agent spawns
  in [`spawn_hand`](../src/hand_spawn.rs) / `spawn_head` — any
  Hand-driven `cargo test` and equivalents must run inside
  `StreamHeartbeat::run` so a silent >5 min test suite cannot kill the
  Hand mid-run.

When adding a new heavy subcommand, land the call-site behind the
wrapper in the same PR that introduces it. Do not stage the subcommand
first and the heartbeat later — the first user who hits the silent
window will lose their session.
