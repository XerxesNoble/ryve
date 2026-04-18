// SPDX-License-Identifier: AGPL-3.0-or-later

//! GitHub REST rate-limiting and exponential backoff.
//!
//! Two responsibilities live here, both pure:
//!
//! 1. **Rate limit header parsing** — GitHub ships a `X-RateLimit-*`
//!    triplet on every response. When `X-RateLimit-Remaining` reaches
//!    zero we must pause until `X-RateLimit-Reset` (UNIX epoch seconds),
//!    otherwise the next request is a guaranteed 403. [`RateLimitInfo`]
//!    parses the triplet from a header-lookup closure so callers can
//!    wire up `reqwest::HeaderMap` or a test fake without this module
//!    depending on reqwest.
//!
//! 2. **Exponential backoff** — On `403` (rate limited / abuse), `429`
//!    (secondary rate limit), or `5xx` (server error), the poller pauses
//!    with an exponential schedule capped at five minutes and jittered
//!    into the [50%, 100%] band of the nominal delay. The cap keeps a
//!    persistent GitHub outage from pushing us into a 20-minute wait;
//!    the jitter prevents synchronised retries from a fleet of Ryve
//!    installs. [`ExponentialBackoff::delay_for`] is the single source
//!    of truth for that schedule.
//!
//! [`classify`] composes the two into the decision the poller actually
//! needs — "should I wait, and for how long?" — given a response status
//! and its headers. All three primitives are deterministic under a fixed
//! `now_epoch` + `jitter` so the integration test can drive them through
//! a scripted clock.

use std::time::Duration;

/// Base delay for the first retry in the exponential schedule.
pub const DEFAULT_BACKOFF_BASE: Duration = Duration::from_secs(1);

/// Upper bound on any single backoff sleep. Five minutes is long enough
/// to absorb a GitHub partial outage without burning quota, short enough
/// that a recovered service is polled again within one cadence window.
pub const DEFAULT_BACKOFF_CAP: Duration = Duration::from_secs(300);

/// Exponential backoff schedule: `min(cap, base * 2^attempt)` scaled by
/// a caller-supplied jitter in `[0, 1)` into the `[50%, 100%]` band.
///
/// `attempt` is zero-indexed — `attempt=0` yields roughly `base` and
/// `attempt=8` saturates the five-minute cap at base=1s. Values above
/// `30` are clamped to avoid shift overflow on 64-bit nanoseconds.
#[derive(Debug, Clone, Copy)]
pub struct ExponentialBackoff {
    base: Duration,
    cap: Duration,
}

impl ExponentialBackoff {
    pub const fn new(base: Duration, cap: Duration) -> Self {
        Self { base, cap }
    }

    /// Default GitHub polling schedule: 1 s base, 5 min cap.
    pub const fn github_default() -> Self {
        Self::new(DEFAULT_BACKOFF_BASE, DEFAULT_BACKOFF_CAP)
    }

    pub fn base(&self) -> Duration {
        self.base
    }

    pub fn cap(&self) -> Duration {
        self.cap
    }

    /// Compute the sleep duration for `attempt` (0-indexed). `jitter`
    /// must be in `[0.0, 1.0)`; values outside that range are clamped.
    /// The returned delay is in the `[50%, 100%]` band of the nominal
    /// exponential step.
    pub fn delay_for(&self, attempt: u32, jitter: f64) -> Duration {
        let jitter = jitter.clamp(0.0, 1.0);
        let shift = attempt.min(30);
        let base_nanos: u128 = self.base.as_nanos();
        let scaled = base_nanos.saturating_mul(1u128 << shift);
        let cap_nanos: u128 = self.cap.as_nanos();
        let capped_nanos = scaled.min(cap_nanos);
        // clamp to u64 for Duration::from_nanos — cap is seconds-scale
        // so this never truncates in practice.
        let capped_u64 = u64::try_from(capped_nanos).unwrap_or(u64::MAX);
        let capped = Duration::from_nanos(capped_u64);
        // Band into [50%, 100%]: half the cap is deterministic, the
        // other half rides on the jitter.
        let factor = 0.5 + 0.5 * jitter;
        Duration::from_secs_f64(capped.as_secs_f64() * factor)
    }
}

/// Parsed GitHub rate-limit triplet plus the optional `Retry-After`
/// header. Any field may be absent — older responses, non-REST
/// endpoints, and 5xx errors frequently omit them.
///
/// Construct via [`RateLimitInfo::from_headers`] with a case-insensitive
/// header-lookup closure. The poller keeps the most recent instance per
/// repo as its gating state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateLimitInfo {
    /// `X-RateLimit-Remaining`: calls left in the current quota window.
    pub remaining: Option<u64>,

    /// `X-RateLimit-Reset`: UNIX epoch seconds when the quota resets.
    pub reset_at_epoch: Option<u64>,

    /// `Retry-After`: seconds the caller must wait before retrying.
    /// Sent on 403/429 responses with secondary rate limits or abuse
    /// detection. Overrides the exponential schedule when present.
    pub retry_after_seconds: Option<u64>,
}

impl RateLimitInfo {
    /// Parse the triplet from a header-lookup closure. Header names are
    /// matched case-insensitively — the closure should already be
    /// normalised, or callers can pass `|n| headers.get(n).cloned()` on
    /// a `reqwest::HeaderMap` which already lowercases.
    pub fn from_headers(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        Self {
            remaining: lookup("x-ratelimit-remaining")
                .as_deref()
                .and_then(|s| s.trim().parse().ok()),
            reset_at_epoch: lookup("x-ratelimit-reset")
                .as_deref()
                .and_then(|s| s.trim().parse().ok()),
            retry_after_seconds: lookup("retry-after")
                .as_deref()
                .and_then(|s| s.trim().parse().ok()),
        }
    }

    /// Return how long to sleep before the next request would be safe
    /// under these rate limits. `None` means "send now".
    ///
    /// Precedence:
    ///
    /// 1. `Retry-After` always wins when present — GitHub's explicit
    ///    "try again in N seconds" directive.
    /// 2. If `remaining == 0` and we have a reset timestamp, wait until
    ///    that epoch.
    /// 3. Otherwise `None` — nothing in the headers forces a wait.
    pub fn wait_before_next(&self, now_epoch: u64) -> Option<Duration> {
        if let Some(secs) = self.retry_after_seconds {
            return Some(Duration::from_secs(secs));
        }
        if matches!(self.remaining, Some(0))
            && let Some(reset) = self.reset_at_epoch
        {
            let wait = reset.saturating_sub(now_epoch);
            return Some(Duration::from_secs(wait));
        }
        None
    }
}

/// Decision the poller derives from a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseOutcome {
    /// 2xx. The payload is usable; the caller may process it and
    /// advance the cursor.
    Proceed,

    /// Transient failure. Sleep for `wait` before retrying. Covers
    /// 403 / 429 (rate limited, possibly with `Retry-After`) and any
    /// 5xx (server error).
    Backoff {
        wait: Duration,
        status: u16,
        reason: BackoffReason,
    },

    /// 4xx that isn't a rate-limit signal. Retrying will not help;
    /// caller should surface the error.
    PermanentFailure { status: u16 },
}

/// Why the poller is sleeping. Used for logging and the integration
/// test assertion that a `Retry-After` response was honoured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackoffReason {
    /// Explicit `Retry-After` directive from GitHub.
    RetryAfter,
    /// `remaining == 0` with a known reset epoch.
    QuotaExhausted,
    /// No explicit directive — using the exponential schedule.
    ExponentialBackoff,
}

/// Fold an HTTP status + rate limit headers into a [`ResponseOutcome`].
///
/// `attempt` is the zero-indexed consecutive-failure count used to
/// scale the exponential schedule. `jitter` must be in `[0, 1)` and is
/// threaded through verbatim so callers can stub a deterministic value
/// in tests.
pub fn classify(
    status: u16,
    info: &RateLimitInfo,
    now_epoch: u64,
    attempt: u32,
    backoff: &ExponentialBackoff,
    jitter: f64,
) -> ResponseOutcome {
    if (200..300).contains(&status) {
        return ResponseOutcome::Proceed;
    }

    // 403 and 429 are both rate-limit signals in GitHub's model:
    // primary limits surface as 403 with remaining=0, secondary limits
    // surface as either 403 or 429 with Retry-After.
    if status == 403 || status == 429 {
        if let Some(wait) = info.wait_before_next(now_epoch) {
            let reason = if info.retry_after_seconds.is_some() {
                BackoffReason::RetryAfter
            } else {
                BackoffReason::QuotaExhausted
            };
            return ResponseOutcome::Backoff {
                wait,
                status,
                reason,
            };
        }
        return ResponseOutcome::Backoff {
            wait: backoff.delay_for(attempt, jitter),
            status,
            reason: BackoffReason::ExponentialBackoff,
        };
    }

    if status >= 500 {
        return ResponseOutcome::Backoff {
            wait: backoff.delay_for(attempt, jitter),
            status,
            reason: BackoffReason::ExponentialBackoff,
        };
    }

    ResponseOutcome::PermanentFailure { status }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_base_is_roughly_one_second_at_attempt_zero() {
        let b = ExponentialBackoff::github_default();
        // attempt=0, jitter=1.0 → full base delay (1s).
        assert_eq!(b.delay_for(0, 1.0), Duration::from_secs(1));
        // jitter=0.0 → floor at half of base (500ms).
        assert_eq!(b.delay_for(0, 0.0), Duration::from_millis(500));
    }

    #[test]
    fn backoff_is_exponential_until_cap() {
        let b = ExponentialBackoff::github_default();
        let d0 = b.delay_for(0, 1.0); // ≈ 1s
        let d1 = b.delay_for(1, 1.0); // ≈ 2s
        let d2 = b.delay_for(2, 1.0); // ≈ 4s
        assert!(d1 > d0);
        assert!(d2 > d1);
        // Large attempt saturates at cap.
        assert_eq!(b.delay_for(30, 1.0), DEFAULT_BACKOFF_CAP);
    }

    #[test]
    fn backoff_respects_cap_even_with_large_attempt() {
        let b = ExponentialBackoff::new(Duration::from_secs(5), Duration::from_secs(60));
        // Even an absurd attempt must not exceed the cap.
        for a in 0..40 {
            let d = b.delay_for(a, 1.0);
            assert!(
                d <= Duration::from_secs(60),
                "attempt={a} produced {d:?} > 60s cap",
            );
        }
    }

    #[test]
    fn jitter_is_within_band() {
        let b = ExponentialBackoff::github_default();
        for attempt in 0..8 {
            let hi = b.delay_for(attempt, 1.0);
            let lo = b.delay_for(attempt, 0.0);
            assert!(lo <= hi, "attempt={attempt}: lo {lo:?} > hi {hi:?}");
            // Band is [50%, 100%] — lo is half of hi.
            let hi_f = hi.as_secs_f64();
            let lo_f = lo.as_secs_f64();
            assert!(
                (hi_f - 2.0 * lo_f).abs() < 1e-6 || hi_f == 0.0,
                "attempt={attempt}: band not 2:1 (lo={lo_f}, hi={hi_f})",
            );
        }
    }

    #[test]
    fn jitter_clamps_out_of_range_values() {
        let b = ExponentialBackoff::github_default();
        // Negative or >1 jitter should fold back into the valid band.
        assert_eq!(b.delay_for(0, -5.0), b.delay_for(0, 0.0));
        assert_eq!(b.delay_for(0, 5.0), b.delay_for(0, 1.0));
    }

    #[test]
    fn rate_limit_info_parses_standard_triplet() {
        let headers = [
            ("x-ratelimit-remaining", "42"),
            ("x-ratelimit-reset", "1700000000"),
        ];
        let info = RateLimitInfo::from_headers(|name| {
            headers
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| (*v).to_string())
        });
        assert_eq!(info.remaining, Some(42));
        assert_eq!(info.reset_at_epoch, Some(1_700_000_000));
        assert_eq!(info.retry_after_seconds, None);
    }

    #[test]
    fn rate_limit_info_parses_retry_after() {
        let info = RateLimitInfo::from_headers(|name| match name {
            "retry-after" => Some("90".into()),
            _ => None,
        });
        assert_eq!(info.retry_after_seconds, Some(90));
        assert_eq!(info.remaining, None);
    }

    #[test]
    fn rate_limit_info_ignores_malformed_values() {
        let info = RateLimitInfo::from_headers(|_| Some("not-a-number".into()));
        assert_eq!(info, RateLimitInfo::default());
    }

    #[test]
    fn wait_before_next_prefers_retry_after() {
        let info = RateLimitInfo {
            remaining: Some(0),
            reset_at_epoch: Some(1_000_000),
            retry_after_seconds: Some(17),
        };
        assert_eq!(info.wait_before_next(0), Some(Duration::from_secs(17)));
    }

    #[test]
    fn wait_before_next_waits_for_reset_when_exhausted() {
        let info = RateLimitInfo {
            remaining: Some(0),
            reset_at_epoch: Some(1_000),
            retry_after_seconds: None,
        };
        assert_eq!(info.wait_before_next(700), Some(Duration::from_secs(300)));
    }

    #[test]
    fn wait_before_next_zero_when_remaining_positive() {
        let info = RateLimitInfo {
            remaining: Some(5),
            reset_at_epoch: Some(9_999_999),
            retry_after_seconds: None,
        };
        assert_eq!(info.wait_before_next(0), None);
    }

    #[test]
    fn wait_before_next_saturates_past_reset() {
        let info = RateLimitInfo {
            remaining: Some(0),
            reset_at_epoch: Some(100),
            retry_after_seconds: None,
        };
        // now > reset: no wait needed.
        assert_eq!(info.wait_before_next(200), Some(Duration::ZERO));
    }

    #[test]
    fn classify_success_proceeds() {
        let out = classify(
            200,
            &RateLimitInfo::default(),
            0,
            0,
            &ExponentialBackoff::github_default(),
            0.5,
        );
        assert_eq!(out, ResponseOutcome::Proceed);
    }

    #[test]
    fn classify_403_with_retry_after_uses_retry_after() {
        let info = RateLimitInfo {
            remaining: None,
            reset_at_epoch: None,
            retry_after_seconds: Some(60),
        };
        let out = classify(403, &info, 0, 0, &ExponentialBackoff::github_default(), 0.5);
        assert!(
            matches!(
                out,
                ResponseOutcome::Backoff {
                    wait,
                    status: 403,
                    reason: BackoffReason::RetryAfter,
                } if wait == Duration::from_secs(60),
            ),
            "unexpected: {out:?}",
        );
    }

    #[test]
    fn classify_403_quota_exhausted_waits_for_reset() {
        let info = RateLimitInfo {
            remaining: Some(0),
            reset_at_epoch: Some(2_000),
            retry_after_seconds: None,
        };
        let out = classify(
            403,
            &info,
            1_500,
            0,
            &ExponentialBackoff::github_default(),
            0.5,
        );
        assert!(
            matches!(
                out,
                ResponseOutcome::Backoff {
                    wait,
                    status: 403,
                    reason: BackoffReason::QuotaExhausted,
                } if wait == Duration::from_secs(500),
            ),
            "unexpected: {out:?}",
        );
    }

    #[test]
    fn classify_429_without_retry_after_uses_exponential() {
        let out = classify(
            429,
            &RateLimitInfo::default(),
            0,
            2,
            &ExponentialBackoff::github_default(),
            1.0,
        );
        assert!(matches!(
            out,
            ResponseOutcome::Backoff {
                status: 429,
                reason: BackoffReason::ExponentialBackoff,
                ..
            },
        ));
    }

    #[test]
    fn classify_500_uses_exponential_backoff() {
        let out = classify(
            502,
            &RateLimitInfo::default(),
            0,
            3,
            &ExponentialBackoff::github_default(),
            1.0,
        );
        assert!(matches!(
            out,
            ResponseOutcome::Backoff {
                status: 502,
                reason: BackoffReason::ExponentialBackoff,
                ..
            },
        ));
    }

    #[test]
    fn classify_404_is_permanent_failure() {
        let out = classify(
            404,
            &RateLimitInfo::default(),
            0,
            0,
            &ExponentialBackoff::github_default(),
            0.5,
        );
        assert_eq!(out, ResponseOutcome::PermanentFailure { status: 404 });
    }
}
