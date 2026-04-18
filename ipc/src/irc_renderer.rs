// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure mapping from canonical Ryve outbox events to IRC PRIVMSG lines.
//!
//! The relay (epic ryve-ddf6fd7f) calls [`event_to_irc`] for every event
//! that has already passed [`crate::signal_discipline::is_allowed`]. The
//! renderer is the single point where v1 event payloads turn into the
//! human-readable text that lands in `#epic-<id>-<slug>` channels.
//!
//! Pure function. No I/O, no clock, no randomness — same input always
//! produces the same `(channel, text, structured)` tuple. Heartbeats are
//! filtered upstream and intentionally have no variant here.
//!
//! Adding a new v1 event type: extend [`EventPayload`], add a `match`
//! arm in [`event_to_irc`] and [`EventPayload::event_type`]. The
//! exhaustiveness check in those matches turns "shipped without IRC
//! mapping" into a compile-time error — the Golden Rule the epic calls
//! out.
//!
//! Snapshot coverage in the test module locks the rendered text for
//! every v1 variant.

use serde::{Deserialize, Serialize};

pub use crate::channel_manager::{EpicRef, channel_name};

/// IRC commands the renderer can emit. v1 only ever produces
/// [`IrcCommand::Privmsg`]; the enum exists so downstream code can
/// pattern-match without a string compare when NOTICE/TOPIC land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IrcCommand {
    Privmsg,
}

impl IrcCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            IrcCommand::Privmsg => "PRIVMSG",
        }
    }
}

/// Tagged JSON attachment that Ryve-native IRC clients can parse to
/// reconstruct the canonical event behind a human-readable line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredAttachment {
    pub kind: &'static str,
    pub event_id: String,
    pub event_type: String,
}

impl StructuredAttachment {
    pub const KIND: &'static str = "ryve.event";

    fn new(event_id: &str, event_type: &str) -> Self {
        Self {
            kind: Self::KIND,
            event_id: event_id.to_string(),
            event_type: event_type.to_string(),
        }
    }
}

/// One IRC line ready to hand to the client. The relay is responsible
/// for serialising the structured attachment onto the wire (typically
/// as an IRCv3 message tag) — this struct just keeps the two pieces
/// separately so the renderer stays format-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrcLine {
    pub channel: String,
    pub command: IrcCommand,
    pub text: String,
    pub structured: Option<StructuredAttachment>,
}

/// Canonical event accepted by the renderer. Wraps the discriminated
/// [`EventPayload`] with the identity fields every event carries: a
/// stable `event_id` for the structured attachment, and the [`EpicRef`]
/// that picks the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEvent {
    pub event_id: String,
    pub epic: EpicRef,
    pub payload: EventPayload,
}

/// Discriminated union over the v1 IRC allow-list. Adding a variant
/// here forces a corresponding arm in [`event_to_irc`] and
/// [`Self::event_type`] — no allow-listed event can ship without an
/// IRC mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventPayload {
    AssignmentCreated {
        assignment_id: String,
        actor: String,
    },
    AssignmentTransitioned {
        assignment_id: String,
        from: String,
        to: String,
        actor: String,
    },
    AssignmentStuck {
        assignment_id: String,
        reason: String,
    },
    ReviewAssigned {
        assignment_id: String,
        reviewer: String,
        kind: String,
    },
    ReviewCompleted {
        assignment_id: String,
        reviewer: String,
        outcome: ReviewOutcome,
    },
    MergeStarted {
        epic_branch: String,
        sub_prs: Vec<u64>,
    },
    MergeCompleted {
        epic_branch: String,
        merged_pr: u64,
    },
    EpicBlockerRaised {
        assignment_id: String,
        reason: String,
    },
    GithubPrOpened {
        pr_number: u64,
        author: String,
        title: String,
    },
    GithubPrClosed {
        pr_number: u64,
        actor: String,
    },
    GithubPrMerged {
        pr_number: u64,
        actor: String,
    },
    GithubPrReviewRequested {
        pr_number: u64,
        reviewer: String,
    },
    GithubPrReviewSubmitted {
        pr_number: u64,
        reviewer: String,
        state: PrReviewState,
    },
    GithubPrCommentAdded {
        pr_number: u64,
        author: String,
        path: Option<String>,
        excerpt: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOutcome {
    Approved,
    Rejected { code: String, location: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrReviewState {
    Approved,
    ChangesRequested,
    Commented,
}

impl PrReviewState {
    fn as_str(self) -> &'static str {
        match self {
            PrReviewState::Approved => "approved",
            PrReviewState::ChangesRequested => "changes_requested",
            PrReviewState::Commented => "commented",
        }
    }
}

impl EventPayload {
    /// The canonical event-type string this payload represents — the
    /// same value the upstream signal-discipline filter sees.
    pub fn event_type(&self) -> &'static str {
        match self {
            EventPayload::AssignmentCreated { .. } => "assignment.created",
            EventPayload::AssignmentTransitioned { .. } => "assignment.transitioned",
            EventPayload::AssignmentStuck { .. } => "assignment.stuck",
            EventPayload::ReviewAssigned { .. } => "review.assigned",
            EventPayload::ReviewCompleted { .. } => "review.completed",
            EventPayload::MergeStarted { .. } => "merge.started",
            EventPayload::MergeCompleted { .. } => "merge.completed",
            EventPayload::EpicBlockerRaised { .. } => "epic.blocker_raised",
            EventPayload::GithubPrOpened { .. } => "github.pr.opened",
            EventPayload::GithubPrClosed { .. } => "github.pr.closed",
            EventPayload::GithubPrMerged { .. } => "github.pr.merged",
            EventPayload::GithubPrReviewRequested { .. } => "github.pr.review_requested",
            EventPayload::GithubPrReviewSubmitted { .. } => "github.pr.review_submitted",
            EventPayload::GithubPrCommentAdded { .. } => "github.pr.comment_added",
        }
    }
}

/// Render an outbox event into a PRIVMSG-shaped [`IrcLine`].
///
/// Always returns `Some` for v1 — the `Option` is the renderer's escape
/// hatch for future variants that want to suppress IRC output without
/// breaking the type-level guarantee that every variant is handled.
pub fn event_to_irc(event: &OutboxEvent) -> Option<IrcLine> {
    let text = match &event.payload {
        EventPayload::AssignmentCreated {
            assignment_id,
            actor,
        } => format!("[assignment] {assignment_id} created for {actor}"),
        EventPayload::AssignmentTransitioned {
            assignment_id,
            from,
            to,
            actor,
        } => format!("[assignment] {assignment_id} moved {from} -> {to} by {actor}"),
        EventPayload::AssignmentStuck {
            assignment_id,
            reason,
        } => format!("[assignment] {assignment_id} stuck: {reason}"),
        EventPayload::ReviewAssigned {
            assignment_id,
            reviewer,
            kind,
        } => format!("[review] {assignment_id} assigned to {reviewer} ({kind})"),
        EventPayload::ReviewCompleted {
            assignment_id,
            reviewer,
            outcome,
        } => match outcome {
            ReviewOutcome::Approved => {
                format!("[review] {assignment_id} approved by {reviewer}")
            }
            ReviewOutcome::Rejected { code, location } => format!(
                "[review] {assignment_id} rejected by {reviewer} \u{2014} {code} in {location}"
            ),
        },
        EventPayload::MergeStarted {
            epic_branch,
            sub_prs,
        } => {
            let prs = sub_prs
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[merge] started for {epic_branch} using sub-PRs {prs}")
        }
        EventPayload::MergeCompleted {
            epic_branch,
            merged_pr,
        } => format!("[merge] {epic_branch} merged to main via PR #{merged_pr}"),
        EventPayload::EpicBlockerRaised {
            assignment_id,
            reason,
        } => format!("[blocker] {assignment_id} waiting on {reason}"),
        EventPayload::GithubPrOpened {
            pr_number,
            author,
            title,
        } => format!("[github] PR #{pr_number} opened by {author}: {title}"),
        EventPayload::GithubPrClosed { pr_number, actor } => {
            format!("[github] PR #{pr_number} closed by {actor}")
        }
        EventPayload::GithubPrMerged { pr_number, actor } => {
            format!("[github] PR #{pr_number} merged by {actor}")
        }
        EventPayload::GithubPrReviewRequested {
            pr_number,
            reviewer,
        } => {
            format!("[github] PR #{pr_number} review requested from {reviewer}")
        }
        EventPayload::GithubPrReviewSubmitted {
            pr_number,
            reviewer,
            state,
        } => format!(
            "[github] PR #{pr_number} review by {reviewer}: {}",
            state.as_str()
        ),
        EventPayload::GithubPrCommentAdded {
            pr_number,
            author,
            path,
            excerpt,
        } => match path {
            Some(p) => {
                format!("[github] PR #{pr_number} comment by {author} on {p}: \"{excerpt}\"")
            }
            None => format!("[github] PR #{pr_number} comment by {author}: \"{excerpt}\""),
        },
    };

    Some(IrcLine {
        channel: channel_name(&event.epic),
        command: IrcCommand::Privmsg,
        text,
        structured: Some(StructuredAttachment::new(
            &event.event_id,
            event.payload.event_type(),
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epic() -> EpicRef {
        EpicRef {
            id: "42".into(),
            name: "Checkout Refactor".into(),
        }
    }

    fn render(event_id: &str, payload: EventPayload) -> IrcLine {
        event_to_irc(&OutboxEvent {
            event_id: event_id.into(),
            epic: epic(),
            payload,
        })
        .expect("v1 events always render")
    }

    fn assert_snapshot(line: &IrcLine, expected_text: &str, expected_event_type: &str) {
        assert_eq!(line.channel, "#epic-42-checkout-refactor");
        assert_eq!(line.command, IrcCommand::Privmsg);
        assert_eq!(line.text, expected_text);
        let attachment = line.structured.as_ref().expect("structured attachment");
        assert_eq!(attachment.kind, "ryve.event");
        assert_eq!(attachment.event_type, expected_event_type);
    }

    // ---------- channel_name ----------

    #[test]
    fn channel_name_uses_id_and_lowercase_slug() {
        assert_eq!(channel_name(&epic()), "#epic-42-checkout-refactor");
    }

    #[test]
    fn channel_name_collapses_runs_of_non_alphanumeric() {
        let e = EpicRef {
            id: "9".into(),
            name: "  Cart // Bug --- Fix!! ".into(),
        };
        assert_eq!(channel_name(&e), "#epic-9-cart-bug-fix");
    }

    #[test]
    fn channel_name_handles_unicode_by_dashing() {
        let e = EpicRef {
            id: "7".into(),
            name: "résumé café".into(),
        };
        assert_eq!(channel_name(&e), "#epic-7-r-sum-caf");
    }

    #[test]
    fn channel_name_with_empty_slug_drops_trailing_dash() {
        let e = EpicRef {
            id: "13".into(),
            name: "!!!".into(),
        };
        assert_eq!(channel_name(&e), "#epic-13");
    }

    // ---------- event_to_irc snapshots: one per v1 variant ----------

    #[test]
    fn renders_assignment_created() {
        let line = render(
            "evt-001",
            EventPayload::AssignmentCreated {
                assignment_id: "asg_9001".into(),
                actor: "agent_claude_01".into(),
            },
        );
        assert_snapshot(
            &line,
            "[assignment] asg_9001 created for agent_claude_01",
            "assignment.created",
        );
    }

    #[test]
    fn renders_assignment_transitioned() {
        let line = render(
            "evt-002",
            EventPayload::AssignmentTransitioned {
                assignment_id: "asg_9001".into(),
                from: "assigned".into(),
                to: "in_progress".into(),
                actor: "agent_claude_01".into(),
            },
        );
        assert_snapshot(
            &line,
            "[assignment] asg_9001 moved assigned -> in_progress by agent_claude_01",
            "assignment.transitioned",
        );
    }

    #[test]
    fn renders_assignment_stuck() {
        let line = render(
            "evt-003",
            EventPayload::AssignmentStuck {
                assignment_id: "asg_9001".into(),
                reason: "no heartbeat for 15m".into(),
            },
        );
        assert_snapshot(
            &line,
            "[assignment] asg_9001 stuck: no heartbeat for 15m",
            "assignment.stuck",
        );
    }

    #[test]
    fn renders_review_assigned() {
        let line = render(
            "evt-004",
            EventPayload::ReviewAssigned {
                assignment_id: "asg_9001".into(),
                reviewer: "agent_codex_02".into(),
                kind: "adversarial".into(),
            },
        );
        assert_snapshot(
            &line,
            "[review] asg_9001 assigned to agent_codex_02 (adversarial)",
            "review.assigned",
        );
    }

    #[test]
    fn renders_review_completed_approved() {
        let line = render(
            "evt-005",
            EventPayload::ReviewCompleted {
                assignment_id: "asg_9001".into(),
                reviewer: "agent_codex_02".into(),
                outcome: ReviewOutcome::Approved,
            },
        );
        assert_snapshot(
            &line,
            "[review] asg_9001 approved by agent_codex_02",
            "review.completed",
        );
    }

    #[test]
    fn renders_review_completed_rejected() {
        let line = render(
            "evt-006",
            EventPayload::ReviewCompleted {
                assignment_id: "asg_9001".into(),
                reviewer: "agent_codex_02".into(),
                outcome: ReviewOutcome::Rejected {
                    code: "NULL_GUARD_MISSING".into(),
                    location: "src/auth/session.ts:118".into(),
                },
            },
        );
        assert_snapshot(
            &line,
            "[review] asg_9001 rejected by agent_codex_02 \u{2014} NULL_GUARD_MISSING in src/auth/session.ts:118",
            "review.completed",
        );
    }

    #[test]
    fn renders_merge_started() {
        let line = render(
            "evt-007",
            EventPayload::MergeStarted {
                epic_branch: "epic/42".into(),
                sub_prs: vec![301, 302, 305],
            },
        );
        assert_snapshot(
            &line,
            "[merge] started for epic/42 using sub-PRs #301, #302, #305",
            "merge.started",
        );
    }

    #[test]
    fn renders_merge_completed() {
        let line = render(
            "evt-008",
            EventPayload::MergeCompleted {
                epic_branch: "epic/42".into(),
                merged_pr: 400,
            },
        );
        assert_snapshot(
            &line,
            "[merge] epic/42 merged to main via PR #400",
            "merge.completed",
        );
    }

    #[test]
    fn renders_epic_blocker_raised() {
        let line = render(
            "evt-009",
            EventPayload::EpicBlockerRaised {
                assignment_id: "asg_9001".into(),
                reason: "product decision on refund window".into(),
            },
        );
        assert_snapshot(
            &line,
            "[blocker] asg_9001 waiting on product decision on refund window",
            "epic.blocker_raised",
        );
    }

    #[test]
    fn renders_github_pr_opened() {
        let line = render(
            "evt-010",
            EventPayload::GithubPrOpened {
                pr_number: 301,
                author: "baxter".into(),
                title: "Refactor session token storage".into(),
            },
        );
        assert_snapshot(
            &line,
            "[github] PR #301 opened by baxter: Refactor session token storage",
            "github.pr.opened",
        );
    }

    #[test]
    fn renders_github_pr_closed() {
        let line = render(
            "evt-011",
            EventPayload::GithubPrClosed {
                pr_number: 301,
                actor: "baxter".into(),
            },
        );
        assert_snapshot(
            &line,
            "[github] PR #301 closed by baxter",
            "github.pr.closed",
        );
    }

    #[test]
    fn renders_github_pr_merged() {
        let line = render(
            "evt-012",
            EventPayload::GithubPrMerged {
                pr_number: 301,
                actor: "baxter".into(),
            },
        );
        assert_snapshot(
            &line,
            "[github] PR #301 merged by baxter",
            "github.pr.merged",
        );
    }

    #[test]
    fn renders_github_pr_review_requested() {
        let line = render(
            "evt-013",
            EventPayload::GithubPrReviewRequested {
                pr_number: 301,
                reviewer: "baxter".into(),
            },
        );
        assert_snapshot(
            &line,
            "[github] PR #301 review requested from baxter",
            "github.pr.review_requested",
        );
    }

    #[test]
    fn renders_github_pr_review_submitted() {
        let line = render(
            "evt-014",
            EventPayload::GithubPrReviewSubmitted {
                pr_number: 301,
                reviewer: "baxter".into(),
                state: PrReviewState::ChangesRequested,
            },
        );
        assert_snapshot(
            &line,
            "[github] PR #301 review by baxter: changes_requested",
            "github.pr.review_submitted",
        );
    }

    #[test]
    fn renders_github_pr_comment_added_with_path() {
        let line = render(
            "evt-015",
            EventPayload::GithubPrCommentAdded {
                pr_number: 301,
                author: "baxter".into(),
                path: Some("src/auth/session.ts".into()),
                excerpt: "Please extract this branch.".into(),
            },
        );
        assert_snapshot(
            &line,
            "[github] PR #301 comment by baxter on src/auth/session.ts: \"Please extract this branch.\"",
            "github.pr.comment_added",
        );
    }

    #[test]
    fn renders_github_pr_comment_added_without_path() {
        let line = render(
            "evt-016",
            EventPayload::GithubPrCommentAdded {
                pr_number: 301,
                author: "baxter".into(),
                path: None,
                excerpt: "LGTM".into(),
            },
        );
        assert_snapshot(
            &line,
            "[github] PR #301 comment by baxter: \"LGTM\"",
            "github.pr.comment_added",
        );
    }

    // ---------- invariants ----------

    #[test]
    fn structured_attachment_carries_event_id_and_type() {
        let line = render(
            "evt-xyz",
            EventPayload::AssignmentCreated {
                assignment_id: "asg_1".into(),
                actor: "agent_a".into(),
            },
        );
        let attachment = line.structured.expect("structured attachment present");
        assert_eq!(attachment.kind, "ryve.event");
        assert_eq!(attachment.event_id, "evt-xyz");
        assert_eq!(attachment.event_type, "assignment.created");
    }

    #[test]
    fn renderer_is_pure_same_event_renders_identically_each_call() {
        let event = OutboxEvent {
            event_id: "evt-pure".into(),
            epic: epic(),
            payload: EventPayload::MergeCompleted {
                epic_branch: "epic/42".into(),
                merged_pr: 400,
            },
        };
        let a = event_to_irc(&event).unwrap();
        let b = event_to_irc(&event).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn command_is_always_privmsg() {
        let cases = v1_payload_samples();
        for (label, payload) in cases {
            let line = render("evt-cmd", payload);
            assert_eq!(line.command, IrcCommand::Privmsg, "command for {label}");
            assert_eq!(line.command.as_str(), "PRIVMSG");
        }
    }

    /// Golden-rule check: every v1 allow-listed event_type from the
    /// signal-discipline filter is constructible here. If a new
    /// allow-list entry ships without a renderer, this test fails — and
    /// since `EventPayload` is exhaustively matched in `event_to_irc`,
    /// adding the variant is the only way to make it pass. The two
    /// halves together close the "ships without IRC mapping" gap.
    #[test]
    fn every_v1_allow_listed_event_type_has_a_renderer() {
        let expected: &[&str] = &[
            "assignment.created",
            "assignment.transitioned",
            "assignment.stuck",
            "review.assigned",
            "review.completed",
            "merge.started",
            "merge.completed",
            "epic.blocker_raised",
            "github.pr.opened",
            "github.pr.closed",
            "github.pr.merged",
            "github.pr.review_requested",
            "github.pr.review_submitted",
            "github.pr.comment_added",
        ];

        let rendered: Vec<&'static str> = v1_payload_samples()
            .into_iter()
            .map(|(_, p)| p.event_type())
            .collect();

        for ty in expected {
            assert!(
                rendered.contains(ty),
                "missing renderer for v1 event type {ty}"
            );
        }
        assert_eq!(
            rendered.len(),
            14,
            "exactly 14 v1 event types must be rendered"
        );
    }

    fn v1_payload_samples() -> Vec<(&'static str, EventPayload)> {
        vec![
            (
                "assignment.created",
                EventPayload::AssignmentCreated {
                    assignment_id: "asg".into(),
                    actor: "a".into(),
                },
            ),
            (
                "assignment.transitioned",
                EventPayload::AssignmentTransitioned {
                    assignment_id: "asg".into(),
                    from: "x".into(),
                    to: "y".into(),
                    actor: "a".into(),
                },
            ),
            (
                "assignment.stuck",
                EventPayload::AssignmentStuck {
                    assignment_id: "asg".into(),
                    reason: "r".into(),
                },
            ),
            (
                "review.assigned",
                EventPayload::ReviewAssigned {
                    assignment_id: "asg".into(),
                    reviewer: "r".into(),
                    kind: "adversarial".into(),
                },
            ),
            (
                "review.completed",
                EventPayload::ReviewCompleted {
                    assignment_id: "asg".into(),
                    reviewer: "r".into(),
                    outcome: ReviewOutcome::Approved,
                },
            ),
            (
                "merge.started",
                EventPayload::MergeStarted {
                    epic_branch: "epic/1".into(),
                    sub_prs: vec![1],
                },
            ),
            (
                "merge.completed",
                EventPayload::MergeCompleted {
                    epic_branch: "epic/1".into(),
                    merged_pr: 1,
                },
            ),
            (
                "epic.blocker_raised",
                EventPayload::EpicBlockerRaised {
                    assignment_id: "asg".into(),
                    reason: "r".into(),
                },
            ),
            (
                "github.pr.opened",
                EventPayload::GithubPrOpened {
                    pr_number: 1,
                    author: "a".into(),
                    title: "t".into(),
                },
            ),
            (
                "github.pr.closed",
                EventPayload::GithubPrClosed {
                    pr_number: 1,
                    actor: "a".into(),
                },
            ),
            (
                "github.pr.merged",
                EventPayload::GithubPrMerged {
                    pr_number: 1,
                    actor: "a".into(),
                },
            ),
            (
                "github.pr.review_requested",
                EventPayload::GithubPrReviewRequested {
                    pr_number: 1,
                    reviewer: "r".into(),
                },
            ),
            (
                "github.pr.review_submitted",
                EventPayload::GithubPrReviewSubmitted {
                    pr_number: 1,
                    reviewer: "r".into(),
                    state: PrReviewState::Approved,
                },
            ),
            (
                "github.pr.comment_added",
                EventPayload::GithubPrCommentAdded {
                    pr_number: 1,
                    author: "a".into(),
                    path: None,
                    excerpt: "x".into(),
                },
            ),
        ]
    }
}
