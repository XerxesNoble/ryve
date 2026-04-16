//! Integration tests for the outbox relay.
//!
//! The relay drains `event_outbox` rows to registered subscribers, stamps
//! `delivered_at` on success, and retries failures with exponential backoff.
//! These tests assert each of those invariants end-to-end against a real
//! sqlx pool (the sqlx::test macro runs migrations on an isolated DB).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use data::sparks::relay::{DeliveryError, OutboxEvent, Relay, RelayConfig, Subscriber};
use futures::future::{BoxFuture, FutureExt};
use sqlx::SqlitePool;

/// Subscriber that records every event it sees.
struct RecordingSubscriber {
    name: &'static str,
    seen: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl RecordingSubscriber {
    fn new(name: &'static str) -> (Arc<Self>, Arc<tokio::sync::Mutex<Vec<String>>>) {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        (
            Arc::new(Self {
                name,
                seen: seen.clone(),
            }),
            seen,
        )
    }
}

impl Subscriber for RecordingSubscriber {
    fn name(&self) -> &'static str {
        self.name
    }

    fn deliver<'a>(&'a self, event: &'a OutboxEvent) -> BoxFuture<'a, Result<(), DeliveryError>> {
        async move {
            self.seen.lock().await.push(event.event_id.clone());
            Ok(())
        }
        .boxed()
    }
}

/// Subscriber that fails the first N attempts for every event, then succeeds.
struct FlakySubscriber {
    name: &'static str,
    fail_until_attempt: u32,
    attempts: Arc<AtomicU32>,
}

impl FlakySubscriber {
    fn new(name: &'static str, fail_until_attempt: u32) -> (Arc<Self>, Arc<AtomicU32>) {
        let attempts = Arc::new(AtomicU32::new(0));
        (
            Arc::new(Self {
                name,
                fail_until_attempt,
                attempts: attempts.clone(),
            }),
            attempts,
        )
    }
}

impl Subscriber for FlakySubscriber {
    fn name(&self) -> &'static str {
        self.name
    }

    fn deliver<'a>(&'a self, _event: &'a OutboxEvent) -> BoxFuture<'a, Result<(), DeliveryError>> {
        async move {
            let n = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if n < self.fail_until_attempt {
                Err(DeliveryError::new(format!("flaky fail #{n}")))
            } else {
                Ok(())
            }
        }
        .boxed()
    }
}

async fn insert_outbox_row(
    pool: &SqlitePool,
    event_id: &str,
    assignment_id: &str,
    event_type: &str,
) {
    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload) \
         VALUES (?, 1, ?, ?, 'actor-1', ?, '{}')",
    )
    .bind(event_id)
    .bind("2026-04-15T10:00:00Z")
    .bind(assignment_id)
    .bind(event_type)
    .execute(pool)
    .await
    .unwrap();
}

async fn delivered_at(pool: &SqlitePool, event_id: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT delivered_at FROM event_outbox WHERE event_id = ?",
    )
    .bind(event_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn fast_config() -> RelayConfig {
    RelayConfig {
        poll_interval: Duration::from_millis(10),
        initial_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(200),
        backoff_multiplier: 2.0,
        batch_size: 100,
    }
}

#[sqlx::test]
async fn delivers_to_all_subscribers_and_stamps_delivered_at(pool: SqlitePool) {
    insert_outbox_row(&pool, "evt-1", "asgn-a", "AssignmentCreated").await;
    insert_outbox_row(&pool, "evt-2", "asgn-a", "PhaseTransitioned").await;

    let (sub_irc, seen_irc) = RecordingSubscriber::new("irc");
    let (sub_gh, seen_gh) = RecordingSubscriber::new("github");

    let relay = Relay::new(pool.clone(), vec![sub_irc, sub_gh], fast_config());

    let outcome = relay.drain_once().await.unwrap();
    assert_eq!(outcome.fetched, 2);
    assert_eq!(outcome.delivered, 2);
    assert_eq!(outcome.failed, 0);
    assert_eq!(outcome.skipped_backoff, 0);

    // Both subscribers saw both events in timestamp order.
    assert_eq!(*seen_irc.lock().await, vec!["evt-1", "evt-2"]);
    assert_eq!(*seen_gh.lock().await, vec!["evt-1", "evt-2"]);

    // delivered_at is stamped for both rows.
    assert!(delivered_at(&pool, "evt-1").await.is_some());
    assert!(delivered_at(&pool, "evt-2").await.is_some());
}

#[sqlx::test]
async fn idempotent_second_pass_is_a_noop(pool: SqlitePool) {
    insert_outbox_row(&pool, "evt-1", "asgn-a", "AssignmentCreated").await;
    let (sub, seen) = RecordingSubscriber::new("irc");

    let relay = Relay::new(pool.clone(), vec![sub], fast_config());

    let first = relay.drain_once().await.unwrap();
    assert_eq!(first.delivered, 1);

    // Second pass must find no undelivered rows and not re-deliver.
    let second = relay.drain_once().await.unwrap();
    assert_eq!(second.fetched, 0);
    assert_eq!(second.delivered, 0);

    assert_eq!(*seen.lock().await, vec!["evt-1"]);
}

#[sqlx::test]
async fn failed_delivery_is_retried_and_eventually_succeeds(pool: SqlitePool) {
    insert_outbox_row(&pool, "evt-1", "asgn-a", "AssignmentCreated").await;

    // Fail the first 2 attempts, succeed on the 3rd.
    let (sub, attempts) = FlakySubscriber::new("flaky", 3);

    let relay = Relay::new(
        pool.clone(),
        vec![sub],
        RelayConfig {
            // Backoff must be short enough for the test to finish quickly.
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(40),
            backoff_multiplier: 2.0,
            ..fast_config()
        },
    );

    // Pass 1: delivery fails, row stays undelivered, retry scheduled.
    let p1 = relay.drain_once().await.unwrap();
    assert_eq!(p1.fetched, 1);
    assert_eq!(p1.failed, 1);
    assert_eq!(p1.delivered, 0);
    assert!(delivered_at(&pool, "evt-1").await.is_none());

    // Immediately running again should skip because backoff hasn't elapsed.
    let p2 = relay.drain_once().await.unwrap();
    assert_eq!(p2.skipped_backoff, 1);
    assert_eq!(p2.failed, 0);
    assert_eq!(p2.delivered, 0);

    // Wait out the first backoff and try again — second attempt still fails.
    tokio::time::sleep(Duration::from_millis(30)).await;
    let p3 = relay.drain_once().await.unwrap();
    assert_eq!(p3.failed, 1);
    assert_eq!(p3.delivered, 0);

    // Wait out the second (doubled) backoff and try again — succeeds.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let p4 = relay.drain_once().await.unwrap();
    assert_eq!(p4.delivered, 1);
    assert_eq!(p4.failed, 0);

    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert!(delivered_at(&pool, "evt-1").await.is_some());
}

#[sqlx::test]
async fn event_is_not_lost_when_any_subscriber_fails(pool: SqlitePool) {
    insert_outbox_row(&pool, "evt-1", "asgn-a", "AssignmentCreated").await;

    // One healthy subscriber, one that permanently fails for this test run.
    let (sub_ok, _seen) = RecordingSubscriber::new("irc");
    let (sub_bad, _attempts) = FlakySubscriber::new("github", u32::MAX);

    let relay = Relay::new(pool.clone(), vec![sub_ok, sub_bad], fast_config());

    for _ in 0..3 {
        let _ = relay.drain_once().await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    // Row is still present and still undelivered — never lost.
    let delivered = delivered_at(&pool, "evt-1").await;
    assert!(
        delivered.is_none(),
        "row must stay undelivered while any subscriber fails, got {delivered:?}"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE event_id = ?")
        .bind("evt-1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn drains_in_timestamp_order(pool: SqlitePool) {
    // Intentionally insert out-of-order.
    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload) \
         VALUES ('evt-b', 1, '2026-04-15T12:00:00Z', 'asgn-a', 'actor', 'X', '{}'),\
                ('evt-a', 1, '2026-04-15T10:00:00Z', 'asgn-a', 'actor', 'X', '{}'),\
                ('evt-c', 1, '2026-04-15T14:00:00Z', 'asgn-a', 'actor', 'X', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (sub, seen) = RecordingSubscriber::new("irc");
    let relay = Relay::new(pool.clone(), vec![sub], fast_config());

    let outcome = relay.drain_once().await.unwrap();
    assert_eq!(outcome.delivered, 3);

    assert_eq!(*seen.lock().await, vec!["evt-a", "evt-b", "evt-c"]);
}
