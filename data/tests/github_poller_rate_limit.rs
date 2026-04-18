// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for the GitHub polling fallback.
//!
//! Exercises the poller against a scripted HTTP fixture so the tests
//! never touch the real GitHub API or the wall clock. Covers the full
//! acceptance of spark `ryve-918412cc` (part of epic [sp-73e42cac]):
//!
//! 1. A normal tick fetches events, feeds them through
//!    [`translator::translate`] + [`applier::apply`], and advances the
//!    per-repo cursor.
//! 2. A `403` response with `Retry-After` drives the poller into a
//!    `Backoff::RetryAfter` outcome — zero further fetches until the
//!    window expires, so GitHub's quota is not exhausted.
//! 3. `remaining=0` in the previous response gates the NEXT tick before
//!    any fetch happens (throttled outcome).
//! 4. 5xx responses trigger the exponential schedule with a capped
//!    delay and bump the consecutive-failure counter.
//! 5. Polling is disabled when `webhook_secret_configured` flips on.
//!
//! [`translator::translate`]: data::github::translator::translate
//! [`applier::apply`]: data::github::applier::apply

use std::cell::RefCell;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use data::github::{
    AppliedOutcome, BackoffReason, ExponentialBackoff, FetchResponse, FetchedEvent, GitHubPayload,
    PollerConfig, RateLimitInfo, TickOutcome,
};
use serde_json::json;

async fn seed_assignment(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO sparks (id, title, description, status, priority, spark_type, \
         workshop_id, metadata, created_at, updated_at) \
         VALUES ('sp-poll', 'poller spark', '', 'open', 2, 'task', 'ws-test', '{}', \
                 '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed spark");

    sqlx::query(
        "INSERT INTO assignments \
         (assignment_id, spark_id, actor_id, session_id, status, role, event_version, \
          assignment_phase, source_branch, target_branch, assigned_at, created_at, updated_at) \
         VALUES ('asgn-poll', 'sp-poll', 'actor-alice', 'sess-alice', 'active', 'owner', 3, \
                 'awaiting_review', 'hand/alice', 'main', \
                 '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed assignment");
}

/// Build a `FetchedEvent` for a PR-opened payload — matches the shape
/// the real poller produces when it converts a REST PR listing into
/// webhook-equivalent translator input.
fn pr_opened_event(github_event_id: &str, pr_number: i64, head_branch: &str) -> FetchedEvent {
    FetchedEvent {
        github_event_id: github_event_id.into(),
        payload: GitHubPayload::new(
            "pull_request",
            json!({
                "action": "opened",
                "pull_request": {
                    "number": pr_number,
                    "head": { "ref": head_branch },
                },
            }),
        ),
    }
}

/// Scripted fetcher. Each call pops the next canned response off the
/// queue and records the `since` cursor the poller passed in. Any call
/// beyond the queue length panics — that is the signal the poller
/// fetched more times than the test expected, which would mean the
/// rate-limit gate is broken.
struct ScriptedFetcher {
    responses: RefCell<Vec<FetchResponse>>,
    calls: RefCell<u32>,
}

impl ScriptedFetcher {
    fn new(responses: Vec<FetchResponse>) -> Self {
        // Reverse so we can `pop` in chronological order.
        let mut v = responses;
        v.reverse();
        Self {
            responses: RefCell::new(v),
            calls: RefCell::new(0),
        }
    }

    fn call_count(&self) -> u32 {
        *self.calls.borrow()
    }

    fn take_next(&self) -> FetchResponse {
        *self.calls.borrow_mut() += 1;
        self.responses
            .borrow_mut()
            .pop()
            .expect("ScriptedFetcher ran out of responses — poller fetched too many times")
    }
}

#[sqlx::test]
async fn poller_proceeds_on_happy_path_and_advances_cursor(pool: sqlx::SqlitePool) {
    seed_assignment(&pool).await;

    let initial_cursor = Utc.with_ymd_and_hms(2026, 4, 18, 0, 0, 0).unwrap();
    let new_cursor = Utc.with_ymd_and_hms(2026, 4, 18, 1, 0, 0).unwrap();
    let fetcher = ScriptedFetcher::new(vec![FetchResponse {
        status: 200,
        rate_limit: RateLimitInfo {
            remaining: Some(4_990),
            reset_at_epoch: Some(2_000_000_000),
            retry_after_seconds: None,
        },
        events: vec![pr_opened_event("gh-poll-001", 101, "hand/alice")],
        observed_cursor: Some(new_cursor),
    }]);

    let mut poller = data::github::Poller::new(
        PollerConfig::new("ryve/ryve").with_cadence(Duration::from_secs(60)),
        initial_cursor,
    );

    let outcome = poller
        .tick(
            &pool,
            |since| {
                assert_eq!(
                    since, initial_cursor,
                    "first tick must fetch from the seeded cursor"
                );
                let response = fetcher.take_next();
                async move { Ok(response) }
            },
            1_000,
            0.5,
        )
        .await
        .expect("tick");

    match outcome {
        TickOutcome::Proceed { cursor, applied } => {
            assert_eq!(cursor, new_cursor, "cursor must advance on success");
            assert_eq!(applied.len(), 1);
            assert!(
                matches!(applied[0], AppliedOutcome::ArtifactRecorded { .. }),
                "applier should have recorded the artifact: {:?}",
                applied[0],
            );
        }
        other => panic!("expected Proceed, got {other:?}"),
    }

    // Assignment must have the artifact mirrored.
    let pr: Option<i64> = sqlx::query_scalar(
        "SELECT github_artifact_pr_number FROM assignments WHERE assignment_id = 'asgn-poll'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pr, Some(101));

    assert_eq!(poller.consecutive_failures(), 0);
    assert_eq!(poller.cursor(), new_cursor);
}

#[sqlx::test]
async fn poller_respects_retry_after_on_403_without_exhausting_quota(pool: sqlx::SqlitePool) {
    // Canonical test: a 403 with Retry-After must NOT trigger further
    // fetches until the window expires. We script a single 403 and
    // assert the poller did not call the fetcher again.
    seed_assignment(&pool).await;

    let initial_cursor = Utc.with_ymd_and_hms(2026, 4, 18, 0, 0, 0).unwrap();
    let fetcher = ScriptedFetcher::new(vec![FetchResponse {
        status: 403,
        rate_limit: RateLimitInfo {
            remaining: Some(0),
            reset_at_epoch: Some(1_000 + 3_600),
            retry_after_seconds: Some(120),
        },
        events: vec![],
        observed_cursor: None,
    }]);

    let mut poller = data::github::Poller::new(
        PollerConfig::new("ryve/ryve").with_cadence(Duration::from_secs(60)),
        initial_cursor,
    );

    // First tick — 403 comes back with Retry-After: 120.
    let outcome = poller
        .tick(
            &pool,
            |_since| {
                let response = fetcher.take_next();
                async move { Ok(response) }
            },
            1_000,
            0.5,
        )
        .await
        .expect("tick");

    match outcome {
        TickOutcome::Backoff {
            wait,
            status,
            reason,
        } => {
            assert_eq!(status, 403);
            assert_eq!(reason, BackoffReason::RetryAfter);
            assert_eq!(
                wait,
                Duration::from_secs(120),
                "Retry-After must set the backoff duration verbatim",
            );
        }
        other => panic!("expected Backoff, got {other:?}"),
    }
    assert_eq!(poller.consecutive_failures(), 1);
    assert_eq!(
        poller.cursor(),
        initial_cursor,
        "cursor must not advance through a failure",
    );

    // Pre-fetch gate: because the previous response left us with a
    // Retry-After window, a second tick BEFORE the window expires must
    // NOT call the fetcher. It should return Throttled. If the poller
    // ignored the gate it would panic by popping an empty queue.
    let outcome = poller
        .tick(
            &pool,
            |_since| {
                let response = fetcher.take_next();
                async move { Ok(response) }
            },
            1_001, // 1 second later — still inside the 120s window.
            0.5,
        )
        .await
        .expect("tick");

    assert!(
        matches!(outcome, TickOutcome::Throttled { .. }),
        "throttled outcome expected, got {outcome:?}",
    );
    // Exactly ONE fetch was made across two ticks — no quota exhaustion.
    assert_eq!(
        fetcher.call_count(),
        1,
        "rate limiter must prevent a second fetch while Retry-After is active",
    );

    // No Assignment state was mutated.
    let pr: Option<i64> = sqlx::query_scalar(
        "SELECT github_artifact_pr_number FROM assignments WHERE assignment_id = 'asgn-poll'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(pr.is_none(), "403 must not produce any DB writes");
}

#[sqlx::test]
async fn poller_5xx_uses_exponential_backoff_and_increments_failure_count(pool: sqlx::SqlitePool) {
    seed_assignment(&pool).await;

    let initial_cursor = Utc.with_ymd_and_hms(2026, 4, 18, 0, 0, 0).unwrap();
    let fetcher = ScriptedFetcher::new(vec![
        FetchResponse {
            status: 502,
            rate_limit: RateLimitInfo::default(),
            events: vec![],
            observed_cursor: None,
        },
        FetchResponse {
            status: 503,
            rate_limit: RateLimitInfo::default(),
            events: vec![],
            observed_cursor: None,
        },
    ]);

    let backoff = ExponentialBackoff::new(Duration::from_secs(2), Duration::from_secs(30));
    let mut poller = data::github::Poller::new(
        PollerConfig::new("ryve/ryve").with_backoff(backoff),
        initial_cursor,
    );

    // First 5xx: attempt=0 → ~[1s, 2s] with jitter=1.0 → exactly 2s.
    let outcome = poller
        .tick(
            &pool,
            |_since| {
                let response = fetcher.take_next();
                async move { Ok(response) }
            },
            1_000,
            1.0,
        )
        .await
        .expect("tick");
    let first_wait = match outcome {
        TickOutcome::Backoff {
            wait,
            status,
            reason,
        } => {
            assert_eq!(status, 502);
            assert_eq!(reason, BackoffReason::ExponentialBackoff);
            wait
        }
        other => panic!("expected Backoff, got {other:?}"),
    };
    assert_eq!(first_wait, Duration::from_secs(2));
    assert_eq!(poller.consecutive_failures(), 1);

    // Second 5xx: attempt=1 → 4s. Advance now_epoch past the first
    // tick's throttle window so the pre-gate lets us fetch again.
    let outcome = poller
        .tick(
            &pool,
            |_since| {
                let response = fetcher.take_next();
                async move { Ok(response) }
            },
            1_010,
            1.0,
        )
        .await
        .expect("tick");
    let second_wait = match outcome {
        TickOutcome::Backoff { wait, .. } => wait,
        other => panic!("expected Backoff, got {other:?}"),
    };
    assert!(
        second_wait > first_wait,
        "exponential backoff must grow: first={first_wait:?} second={second_wait:?}",
    );
    assert_eq!(poller.consecutive_failures(), 2);
}

#[sqlx::test]
async fn poller_disabled_when_webhook_configured(pool: sqlx::SqlitePool) {
    seed_assignment(&pool).await;

    let initial_cursor = Utc.with_ymd_and_hms(2026, 4, 18, 0, 0, 0).unwrap();
    // No canned responses — if the poller tries to fetch, this panics.
    let fetcher = ScriptedFetcher::new(vec![]);

    let mut poller = data::github::Poller::new(
        PollerConfig::new("ryve/ryve").with_webhook_configured(true),
        initial_cursor,
    );

    let outcome = poller
        .tick(
            &pool,
            |_since| {
                let response = fetcher.take_next();
                async move { Ok(response) }
            },
            0,
            0.5,
        )
        .await
        .expect("tick");

    assert!(matches!(outcome, TickOutcome::Disabled));
    assert_eq!(fetcher.call_count(), 0);
}

#[sqlx::test]
async fn poller_cursor_holds_on_transient_failure(pool: sqlx::SqlitePool) {
    // A 403 followed by a successful fetch must re-fetch from the
    // same since timestamp on the successful tick — the cursor only
    // advances on Proceed.
    seed_assignment(&pool).await;

    let initial_cursor = Utc.with_ymd_and_hms(2026, 4, 18, 0, 0, 0).unwrap();
    let advanced_cursor = Utc.with_ymd_and_hms(2026, 4, 18, 2, 0, 0).unwrap();

    let fetcher = ScriptedFetcher::new(vec![
        FetchResponse {
            status: 429,
            rate_limit: RateLimitInfo {
                remaining: Some(0),
                reset_at_epoch: Some(1_100),
                retry_after_seconds: Some(10),
            },
            events: vec![],
            observed_cursor: None,
        },
        FetchResponse {
            status: 200,
            rate_limit: RateLimitInfo {
                remaining: Some(100),
                reset_at_epoch: Some(2_000),
                retry_after_seconds: None,
            },
            events: vec![pr_opened_event("gh-poll-002", 202, "hand/alice")],
            observed_cursor: Some(advanced_cursor),
        },
    ]);

    let mut poller = data::github::Poller::new(PollerConfig::new("ryve/ryve"), initial_cursor);

    // First tick: 429 + Retry-After.
    let _ = poller
        .tick(
            &pool,
            |since| {
                assert_eq!(since, initial_cursor);
                let r = fetcher.take_next();
                async move { Ok(r) }
            },
            1_000,
            0.5,
        )
        .await
        .expect("tick 1");
    assert_eq!(poller.cursor(), initial_cursor);

    // Second tick: "now" is past the Retry-After window so the pre-gate
    // does NOT throttle — fetcher is called and returns 200. The
    // fetched cursor advances; failures counter resets.
    let outcome = poller
        .tick(
            &pool,
            |since| {
                assert_eq!(
                    since, initial_cursor,
                    "retry after failure must use the un-advanced cursor",
                );
                let r = fetcher.take_next();
                async move { Ok(r) }
            },
            1_200, // > 1_000 + 10 — window expired
            0.5,
        )
        .await
        .expect("tick 2");

    assert!(matches!(outcome, TickOutcome::Proceed { .. }));
    assert_eq!(poller.cursor(), advanced_cursor);
    assert_eq!(poller.consecutive_failures(), 0);
    assert_eq!(fetcher.call_count(), 2);
}
