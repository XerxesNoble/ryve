// SPDX-License-Identifier: AGPL-3.0-or-later

//! REST polling fallback for the GitHub mirror.
//!
//! Webhook ingestion is the preferred path — it is low-latency, carries
//! a signed delivery id, and does not burn quota. Many deployments,
//! however, cannot expose an HTTPS endpoint. This module provides a
//! timed polling loop that fetches the same event vocabulary (PRs,
//! reviews, issue comments, check runs) from the REST API, pushes each
//! response through [`translator::translate`] and then
//! [`applier::apply`], and advances a per-repo cursor so the next tick
//! only asks for what is new.
//!
//! # Fetch abstraction
//!
//! The actual HTTP request lives outside this module so the poller is
//! unit-testable without a network. Callers pass an async closure
//! matching [`tick`]'s signature — the real caller wraps a reqwest
//! client; the integration test at
//! `data/tests/github_poller_rate_limit.rs` passes a scripted sequence
//! of canned responses so it can exercise 403 + Retry-After without
//! touching the network or the wall clock.
//!
//! # Rate limiting
//!
//! Every [`tick`] consults [`RateLimitInfo::wait_before_next`] before
//! issuing its fetch. The classification of the response is delegated
//! to [`rate_limit::classify`], which returns a
//! [`ResponseOutcome`]. `Backoff` outcomes bump the consecutive-failure
//! counter and the returned duration is what the caller should sleep
//! before re-ticking; `Proceed` outcomes reset the counter and advance
//! the cursor; `PermanentFailure` surfaces the error.
//!
//! # Webhook gate
//!
//! [`PollerConfig::webhook_secret_configured`] flips the entire loop
//! to a no-op: [`run_forever`] returns immediately, and [`tick`] is a
//! documented `Ok(TickOutcome::Disabled)` so callers that maintain their
//! own scheduling loop can inspect the state without branching on the
//! config themselves.
//!
//! [`translator::translate`]: super::translator::translate
//! [`applier::apply`]: super::applier::apply
//! [`RateLimitInfo::wait_before_next`]: super::rate_limit::RateLimitInfo::wait_before_next
//! [`rate_limit::classify`]: super::rate_limit::classify
//! [`ResponseOutcome`]: super::rate_limit::ResponseOutcome

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use super::applier::{self, AppliedOutcome, ApplyError, GithubEventsSeenRepo};
use super::rate_limit::{
    BackoffReason, ExponentialBackoff, RateLimitInfo, ResponseOutcome, classify,
};
use super::translator::{self, GitHubPayload, TranslateError};

/// Default cadence between polling ticks when the caller does not
/// override it. 60 s matches the GitHub REST guidance for conditional
/// requests and keeps the baseline consumption well below the 5000
/// req/hr core-API quota for any reasonable repo count.
pub const DEFAULT_POLL_CADENCE: Duration = Duration::from_secs(60);

/// Polling configuration for one Epic repo.
///
/// The poller is stateful per-repo: `cursor` advances on every
/// successful tick so only events newer than the last poll are
/// fetched. `webhook_secret_configured` is the single flag that
/// decides whether the loop runs at all — when a webhook is wired up
/// we must not double-ingest.
#[derive(Debug, Clone)]
pub struct PollerConfig {
    /// GitHub repo the poller targets, in `owner/repo` form. Used
    /// only for log prefixing — the fetcher closure owns the actual
    /// URL construction.
    pub repo_full_name: String,

    /// Wall-clock time between successive `tick`s on the happy path.
    /// Overridden upward by rate-limit headers and exponential
    /// backoff. Defaults to [`DEFAULT_POLL_CADENCE`].
    pub cadence: Duration,

    /// When `true` the poller is disabled — webhook ingestion is
    /// authoritative and polling would double-insert events. The
    /// applier's dedup log would catch that, but avoiding the
    /// redundant fetch also saves quota.
    pub webhook_secret_configured: bool,

    /// Backoff schedule used on 403/429/5xx. Caller can inject a
    /// faster schedule for tests; production uses
    /// [`ExponentialBackoff::github_default`].
    pub backoff: ExponentialBackoff,
}

impl PollerConfig {
    /// Build a config for `repo_full_name` with defaults: 60 s cadence,
    /// webhooks disabled, GitHub-standard backoff.
    pub fn new(repo_full_name: impl Into<String>) -> Self {
        Self {
            repo_full_name: repo_full_name.into(),
            cadence: DEFAULT_POLL_CADENCE,
            webhook_secret_configured: false,
            backoff: ExponentialBackoff::github_default(),
        }
    }

    pub fn with_cadence(mut self, cadence: Duration) -> Self {
        self.cadence = cadence;
        self
    }

    pub fn with_webhook_configured(mut self, configured: bool) -> Self {
        self.webhook_secret_configured = configured;
        self
    }

    pub fn with_backoff(mut self, backoff: ExponentialBackoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// `true` when polling is live. [`run_forever`] and [`tick`] use
    /// this as the single gate — never branch on
    /// `webhook_secret_configured` directly at call sites.
    pub fn is_enabled(&self) -> bool {
        !self.webhook_secret_configured
    }
}

/// One event the poller is handing to the applier. `github_event_id`
/// is the dedup key and must be stable across webhook and poll paths
/// for the same underlying GitHub event — for REST this is typically
/// a composite like `"pr-101-review-4423"` built by the fetcher.
#[derive(Debug, Clone)]
pub struct FetchedEvent {
    pub github_event_id: String,
    pub payload: GitHubPayload,
}

/// Shape of a single fetch response the poller's tick consumes.
///
/// `events` is already deduped on the wire side (the fetcher is
/// expected to merge paginated pages before returning); the applier
/// deduplicates against prior deliveries via its own `github_events_seen`
/// table.
#[derive(Debug)]
pub struct FetchResponse {
    pub status: u16,
    pub rate_limit: RateLimitInfo,
    pub events: Vec<FetchedEvent>,
    /// When the fetcher itself advances an internal cursor (e.g. the
    /// `updated_at` of the last PR it saw), it should return that time
    /// here so the poller can store it. `None` means "use the tick
    /// start time" — safe default for repos with zero events.
    pub observed_cursor: Option<DateTime<Utc>>,
}

/// Errors from the fetch-and-apply pipeline. The fetch side is
/// represented as an opaque string so the poller does not depend on
/// reqwest. Apply and translate errors propagate their own types.
#[derive(Debug, thiserror::Error)]
pub enum PollError {
    #[error("fetch failed: {0}")]
    Fetch(String),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("apply failed: {0}")]
    Apply(#[from] ApplyError),

    #[error("permanent HTTP failure: status {status}")]
    PermanentFailure { status: u16 },
}

/// Observable outcome of one `tick`. The caller uses this to decide
/// how long to sleep before the next tick — `Proceed` → `cadence`,
/// `Backoff` → the returned duration, `Disabled` → exit the loop.
#[derive(Debug)]
pub enum TickOutcome {
    /// Fetch succeeded. Cursor advanced to `cursor`. `applied` is the
    /// per-event outcome in order; empty vector means nothing new.
    Proceed {
        cursor: DateTime<Utc>,
        applied: Vec<AppliedOutcome>,
    },

    /// Response indicated we should slow down. Caller should sleep at
    /// least `wait` before the next tick. The cursor did NOT advance;
    /// the next successful tick will re-fetch from the same `since`.
    Backoff {
        wait: Duration,
        status: u16,
        reason: BackoffReason,
    },

    /// Headers said "you're out of quota" even before the fetch. The
    /// poller did not call the fetcher; caller should sleep `wait`.
    Throttled { wait: Duration },

    /// `webhook_secret_configured` is set. Polling is intentionally a
    /// no-op. Caller should exit its run loop.
    Disabled,
}

/// One repo's polling state: the cursor, the rate-limit snapshot from
/// the previous tick, the consecutive-failure counter that drives
/// exponential backoff, and an absolute deadline (in epoch seconds)
/// before which the next tick is a guaranteed no-op.
///
/// `throttled_until_epoch` is the single authority on "are we allowed
/// to fetch right now?". It is set whenever a response returns a
/// backoff duration (either Retry-After, quota exhaustion, or the
/// exponential schedule on 5xx) so the pre-gate on the next tick can
/// short-circuit without reaching the fetcher — even if the
/// `RateLimitInfo` in the response would otherwise fail to encode the
/// deadline as an absolute wall-clock time.
#[derive(Debug)]
pub struct Poller {
    config: PollerConfig,
    cursor: DateTime<Utc>,
    rate_limit: RateLimitInfo,
    throttled_until_epoch: Option<u64>,
    consecutive_failures: u32,
    seen: GithubEventsSeenRepo,
}

impl Poller {
    /// Create a fresh poller for `config`, seeded with `initial_cursor`
    /// as the "since" for the first tick.
    pub fn new(config: PollerConfig, initial_cursor: DateTime<Utc>) -> Self {
        Self {
            config,
            cursor: initial_cursor,
            rate_limit: RateLimitInfo::default(),
            throttled_until_epoch: None,
            consecutive_failures: 0,
            seen: GithubEventsSeenRepo::new(),
        }
    }

    pub fn config(&self) -> &PollerConfig {
        &self.config
    }

    pub fn cursor(&self) -> DateTime<Utc> {
        self.cursor
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    pub fn rate_limit(&self) -> RateLimitInfo {
        self.rate_limit
    }

    /// Absolute epoch-second deadline before which [`tick`] will
    /// short-circuit to [`TickOutcome::Throttled`] without invoking the
    /// fetcher. `None` means no active throttle.
    pub fn throttled_until(&self) -> Option<u64> {
        self.throttled_until_epoch
    }

    /// Run one polling cycle.
    ///
    /// `fetch` receives the current cursor and returns a [`FetchResponse`]
    /// (or a raw transport error). `now_epoch` and `jitter` are
    /// injected so tests can drive the rate-limit math deterministically
    /// — production callers pass [`current_epoch`] and a fresh random
    /// value from [`default_jitter`].
    pub async fn tick<F, Fut>(
        &mut self,
        pool: &SqlitePool,
        fetch: F,
        now_epoch: u64,
        jitter: f64,
    ) -> Result<TickOutcome, PollError>
    where
        F: FnOnce(DateTime<Utc>) -> Fut,
        Fut: Future<Output = Result<FetchResponse, String>>,
    {
        if !self.config.is_enabled() {
            return Ok(TickOutcome::Disabled);
        }

        // Pre-fetch gate: either the previous tick parked us until a
        // known absolute deadline (set from Retry-After / 5xx backoff),
        // or the last observed rate-limit headers say remaining=0 and
        // the reset window has not elapsed. Either way, don't fetch.
        if let Some(until) = self.throttled_until_epoch
            && now_epoch < until
        {
            return Ok(TickOutcome::Throttled {
                wait: Duration::from_secs(until - now_epoch),
            });
        }
        if let Some(wait) = self.rate_limit.wait_before_next(now_epoch)
            && !wait.is_zero()
        {
            return Ok(TickOutcome::Throttled { wait });
        }

        let response = fetch(self.cursor).await.map_err(PollError::Fetch)?;

        let outcome = classify(
            response.status,
            &response.rate_limit,
            now_epoch,
            self.consecutive_failures,
            &self.config.backoff,
            jitter,
        );

        // Always update our rate-limit snapshot — even an error carries
        // useful remaining/reset headers for the next tick's pre-gate.
        // Retry-After is a *delta* from when we received the header; we
        // strip it here so subsequent ticks don't re-apply the same
        // delta as if "now" had not moved — the absolute deadline lives
        // in `throttled_until_epoch`.
        self.rate_limit = RateLimitInfo {
            retry_after_seconds: None,
            ..response.rate_limit
        };

        match outcome {
            ResponseOutcome::Proceed => {
                self.consecutive_failures = 0;
                self.throttled_until_epoch = None;
                let applied = apply_events(pool, &response.events, &self.seen).await?;
                // When the fetcher reports no observed_cursor (e.g. an
                // empty repo with zero events on this tick), advance to
                // the tick start time. Without this fallback the cursor
                // would never advance on a successful empty fetch and
                // every subsequent tick would re-query the same `since`,
                // wasting rate-limit budget and never narrowing the
                // window. The documented contract on
                // `FetchResponse.observed_cursor` is "None means use the
                // tick start time".
                let tick_start_cursor =
                    DateTime::<Utc>::from_timestamp(now_epoch as i64, 0).unwrap_or(self.cursor);
                let new_cursor = response.observed_cursor.unwrap_or(tick_start_cursor);
                if new_cursor > self.cursor {
                    self.cursor = new_cursor;
                }
                Ok(TickOutcome::Proceed {
                    cursor: self.cursor,
                    applied,
                })
            }
            ResponseOutcome::Backoff {
                wait,
                status,
                reason,
            } => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                self.throttled_until_epoch = Some(now_epoch.saturating_add(wait.as_secs()));
                Ok(TickOutcome::Backoff {
                    wait,
                    status,
                    reason,
                })
            }
            ResponseOutcome::PermanentFailure { status } => {
                Err(PollError::PermanentFailure { status })
            }
        }
    }
}

/// Translate + apply each fetched event in its own transaction. The
/// applier's contract requires the Tx to be committed even on validator
/// rejection so the warning row and the seen-marker persist — we honor
/// that here and surface the error to the caller afterward.
async fn apply_events(
    pool: &SqlitePool,
    events: &[FetchedEvent],
    seen: &GithubEventsSeenRepo,
) -> Result<Vec<AppliedOutcome>, PollError> {
    let mut outcomes = Vec::with_capacity(events.len());
    for ev in events {
        let canonical = match translator::translate(&ev.payload) {
            Ok(c) => c,
            Err(TranslateError::Unsupported(_)) => {
                // Unsupported events are a documented log-and-drop at
                // the ingress edge; we skip silently so a single stray
                // `push` delivery cannot derail the whole tick.
                continue;
            }
            Err(e) => return Err(PollError::Fetch(format!("translate: {e}"))),
        };

        let mut tx = pool.begin().await?;
        let result = applier::apply(&mut tx, &ev.github_event_id, &canonical, seen).await;
        tx.commit().await?;
        outcomes.push(result?);
    }
    Ok(outcomes)
}

/// Current UNIX epoch in seconds. Wrapper so tests can substitute a
/// scripted clock without reaching into `SystemTime` directly.
pub fn current_epoch() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Jitter in `[0, 1)` for the exponential backoff formula. Sourced
/// from the low 64 bits of a v4 UUID so we do not have to pull in
/// `rand` just for one float per retry.
pub fn default_jitter() -> f64 {
    let u = uuid::Uuid::new_v4().as_u128();
    let bits = (u & 0xFFFF_FFFF_FFFF_FFFF) as u64;
    (bits as f64) / (u64::MAX as f64 + 1.0)
}

/// Drive `poller` forever on a tokio runtime, sleeping `cadence`
/// between successful ticks and `wait` on backoff / throttled
/// responses. Returns only when the config is disabled or the fetch
/// surfaces a permanent failure.
///
/// Callers that need finer control (per-tick metrics, graceful
/// shutdown, external cancellation) should drive [`Poller::tick`]
/// directly from their own loop.
pub async fn run_forever<F, Fut>(
    poller: &mut Poller,
    pool: &SqlitePool,
    mut fetch: F,
) -> Result<(), PollError>
where
    F: FnMut(DateTime<Utc>) -> Fut,
    Fut: Future<Output = Result<FetchResponse, String>>,
{
    if !poller.config.is_enabled() {
        return Ok(());
    }

    loop {
        let outcome = poller
            .tick(pool, &mut fetch, current_epoch(), default_jitter())
            .await?;
        let sleep = match outcome {
            TickOutcome::Disabled => return Ok(()),
            TickOutcome::Proceed { .. } => poller.config.cadence,
            TickOutcome::Backoff { wait, .. } | TickOutcome::Throttled { wait } => wait,
        };
        tokio::time::sleep(sleep).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sane() {
        let cfg = PollerConfig::new("ryve/ryve");
        assert_eq!(cfg.cadence, DEFAULT_POLL_CADENCE);
        assert!(!cfg.webhook_secret_configured);
        assert!(cfg.is_enabled());
    }

    #[test]
    fn config_disables_when_webhook_configured() {
        let cfg = PollerConfig::new("ryve/ryve").with_webhook_configured(true);
        assert!(!cfg.is_enabled());
    }

    #[test]
    fn config_builder_threads_cadence() {
        let cfg = PollerConfig::new("ryve/ryve").with_cadence(Duration::from_secs(10));
        assert_eq!(cfg.cadence, Duration::from_secs(10));
    }

    #[test]
    fn default_jitter_is_in_unit_interval() {
        for _ in 0..64 {
            let j = default_jitter();
            assert!((0.0..1.0).contains(&j), "jitter {j} out of range");
        }
    }
}
