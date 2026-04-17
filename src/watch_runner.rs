// SPDX-License-Identifier: AGPL-3.0-or-later

//! Background tokio task that drives the durable watch scheduler.
//!
//! Spark ryve-6ab1980c [sp-ee3f5c74]: this is the runtime wrapper. The
//! actual transactional tick logic — the half that has to be atomic and
//! restart-safe — lives in [`data::sparks::watch_runner`]. This module
//! only owns the timer, the shutdown channel, and the join handle.
//!
//! Lifecycle: [`spawn`] is called from `src/app.rs` when a workshop
//! becomes ready (its sqlx pool is open), and the returned
//! [`WatchRunnerHandle`] is stored on the `Workshop` so that
//! [`WatchRunnerHandle::shutdown`] can be awaited when the workshop is
//! closed. Shutdown lets the currently in-flight tick finish before the
//! task returns, so mid-tick writes commit cleanly.

use std::time::Duration;

use data::sparks::watch_runner as core;
use sqlx::SqlitePool;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Default scheduler tick interval — every N seconds the runner wakes up
/// and scans for due watches. Overridable via `RYVE_WATCH_TICK_SECS`.
pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(10);

/// Read the effective tick interval from the environment. Falls back to
/// [`DEFAULT_TICK_INTERVAL`] when the variable is absent, empty, or not
/// a positive integer — we never let a bogus value collapse to 0 and
/// spin the scheduler.
pub fn tick_interval_from_env() -> Duration {
    std::env::var("RYVE_WATCH_TICK_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TICK_INTERVAL)
}

/// Handle to a running watch scheduler. Dropping this handle leaves the
/// task running; call [`shutdown`](WatchRunnerHandle::shutdown) to stop
/// it cleanly.
pub struct WatchRunnerHandle {
    stop_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl WatchRunnerHandle {
    /// Signal shutdown and await the task. Any in-flight tick finishes
    /// first — the stop signal is only observed at the outer `select!`
    /// point between ticks, so mid-tick DB writes commit before the
    /// task returns.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            // Receiver dropped means the task already exited; that's fine.
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            match join.await {
                Ok(()) => {}
                Err(e) if e.is_cancelled() => {}
                Err(e) => log::warn!("watch runner panicked during shutdown: {e}"),
            }
        }
    }
}

/// Spawn the watch scheduler as a tokio task. Returns a handle that
/// callers must store so they can signal shutdown — dropping the handle
/// without calling [`WatchRunnerHandle::shutdown`] is supported (the
/// task will exit when the process dies) but will not await graceful
/// completion.
pub fn spawn(pool: SqlitePool, tick_interval: Duration) -> WatchRunnerHandle {
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(tick_interval);
        // `Delay` skips the immediate tick-on-start and delays any
        // missed ticks rather than replaying them — the underlying
        // `tick()` already catches up on missed fires via its own
        // schedule math, so we don't need the timer to do it too.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Discard the first tick which fires immediately on `interval`.
        ticker.tick().await;
        loop {
            tokio::select! {
                biased;
                _ = &mut stop_rx => break,
                _ = ticker.tick() => {
                    let now = chrono::Utc::now();
                    match core::tick(&pool, now).await {
                        Ok(outcome) if outcome.fired > 0
                            || outcome.completed > 0
                            || outcome.skipped > 0 => {
                            log::debug!(
                                "watch runner: fired={} completed={} skipped={}",
                                outcome.fired, outcome.completed, outcome.skipped
                            );
                        }
                        Ok(_) => {}
                        Err(e) => log::warn!("watch runner tick error: {e}"),
                    }
                }
            }
        }
    });
    WatchRunnerHandle {
        stop_tx: Some(stop_tx),
        join: Some(join),
    }
}
