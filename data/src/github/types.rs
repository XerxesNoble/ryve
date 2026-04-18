// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shared GitHub mirror types.
//!
//! These types are the stable boundary between GitHub-facing code
//! (webhook/poll ingestors, the applier) and the rest of the workgraph.
//! Raw `octocrab` payloads are translated into [`CanonicalGitHubEvent`]
//! at the edge and everything downstream speaks this canonical form —
//! that way the applier's decision table is a pure function of our
//! enum, not of whatever shape GitHub ships this week.
//!
//! [`GitHubArtifactRef`] is the persisted link from an `assignments`
//! row to the PR it mirrors; it corresponds 1:1 to the
//! `assignments.github_artifact_branch` + `github_artifact_pr_number`
//! columns introduced by migration `021_github_mirror.sql`.

use serde::{Deserialize, Serialize};

/// Canonical, provider-agnostic shape of a single GitHub event relevant
/// to the mirror. Every ingress path (webhook delivery, REST polling,
/// manual replay) normalizes into one of these variants before the
/// applier looks at it.
///
/// The variants deliberately mirror the vocabulary the applier needs
/// to drive phase transitions — *not* GitHub's full webhook taxonomy.
/// New kinds of signal get a new variant here and a matching arm in
/// the applier; unknown GitHub events are dropped at the ingress edge
/// and never reach this enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanonicalGitHubEvent {
    /// A pull request was opened. Carries the freshly-assigned PR
    /// number and the head branch so the applier can link it to the
    /// owning assignment.
    PrOpened { pr_number: i64, head_branch: String },

    /// A pull request's metadata or head commit was updated
    /// (title/body edits, force-push, new commits, etc.). Treated as a
    /// generic "something changed" signal — the applier re-reads state
    /// rather than diffing.
    PrUpdated { pr_number: i64, head_branch: String },

    /// A review arrived with the `APPROVED` state. Advances the
    /// assignment toward `approved` once all required reviewers are in.
    ReviewApproved { pr_number: i64, reviewer: String },

    /// A review arrived with the `CHANGES_REQUESTED` state. Forces the
    /// assignment back into `in_repair`.
    ReviewChangesRequested { pr_number: i64, reviewer: String },

    /// A PR-level comment (issue comment on the PR thread, not a line
    /// comment). The applier currently uses these only for audit
    /// context, but they are carried through so downstream consumers
    /// (notifications, UI) see the same event stream.
    PrComment {
        pr_number: i64,
        author: String,
        body: String,
    },

    /// A GitHub Actions check run transitioned. `status` is GitHub's
    /// `conclusion` field when the run has finished, otherwise its
    /// in-progress `status`.
    CheckRunStatus {
        pr_number: i64,
        check_name: String,
        status: String,
    },

    /// The pull request was merged. Terminal: the applier drives the
    /// assignment to `merged` and stops reacting to further events.
    PrMerged {
        pr_number: i64,
        merge_commit_sha: String,
    },

    /// The pull request was closed without merging. Terminal in the
    /// opposite direction: the applier records the close but does not
    /// advance to `merged`.
    PrClosed { pr_number: i64 },
}

impl CanonicalGitHubEvent {
    /// Stable discriminator used for logging, metrics, and the
    /// `github_events_seen.event_type` column. Matches the `"kind"`
    /// tag emitted by serde so UI/DB and in-memory values agree.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::PrOpened { .. } => "pr_opened",
            Self::PrUpdated { .. } => "pr_updated",
            Self::ReviewApproved { .. } => "review_approved",
            Self::ReviewChangesRequested { .. } => "review_changes_requested",
            Self::PrComment { .. } => "pr_comment",
            Self::CheckRunStatus { .. } => "check_run_status",
            Self::PrMerged { .. } => "pr_merged",
            Self::PrClosed { .. } => "pr_closed",
        }
    }

    /// The PR number every variant carries. Centralised so callers
    /// that only need routing don't have to pattern-match over every
    /// arm.
    pub fn pr_number(&self) -> i64 {
        match self {
            Self::PrOpened { pr_number, .. }
            | Self::PrUpdated { pr_number, .. }
            | Self::ReviewApproved { pr_number, .. }
            | Self::ReviewChangesRequested { pr_number, .. }
            | Self::PrComment { pr_number, .. }
            | Self::CheckRunStatus { pr_number, .. }
            | Self::PrMerged { pr_number, .. }
            | Self::PrClosed { pr_number } => *pr_number,
        }
    }
}

/// Persisted link from one `assignments` row to the GitHub artifact
/// (head branch + PR number) that mirrors it. Mirrors the
/// `github_artifact_branch` / `github_artifact_pr_number` columns
/// added by migration 019; both are present together or not at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubArtifactRef {
    pub branch: String,
    pub pr_number: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_event_kind_matches_serde_tag() {
        // The `kind()` helper must agree with serde's `"kind"` tag so
        // logs, metrics, and the `github_events_seen.event_type`
        // column all use the same string.
        let cases = [
            CanonicalGitHubEvent::PrOpened {
                pr_number: 1,
                head_branch: "feat/x".into(),
            },
            CanonicalGitHubEvent::PrUpdated {
                pr_number: 1,
                head_branch: "feat/x".into(),
            },
            CanonicalGitHubEvent::ReviewApproved {
                pr_number: 1,
                reviewer: "alice".into(),
            },
            CanonicalGitHubEvent::ReviewChangesRequested {
                pr_number: 1,
                reviewer: "alice".into(),
            },
            CanonicalGitHubEvent::PrComment {
                pr_number: 1,
                author: "alice".into(),
                body: "lgtm".into(),
            },
            CanonicalGitHubEvent::CheckRunStatus {
                pr_number: 1,
                check_name: "ci".into(),
                status: "success".into(),
            },
            CanonicalGitHubEvent::PrMerged {
                pr_number: 1,
                merge_commit_sha: "abc".into(),
            },
            CanonicalGitHubEvent::PrClosed { pr_number: 1 },
        ];

        for ev in cases {
            let json = serde_json::to_value(&ev).unwrap();
            let tag = json["kind"].as_str().expect("kind tag present");
            assert_eq!(ev.kind(), tag, "kind() must match serde tag for {ev:?}",);
        }
    }

    #[test]
    fn canonical_event_round_trips_through_serde() {
        let ev = CanonicalGitHubEvent::ReviewApproved {
            pr_number: 42,
            reviewer: "bob".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: CanonicalGitHubEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn pr_number_is_uniform_across_variants() {
        assert_eq!(
            CanonicalGitHubEvent::PrClosed { pr_number: 7 }.pr_number(),
            7,
        );
        assert_eq!(
            CanonicalGitHubEvent::PrMerged {
                pr_number: 99,
                merge_commit_sha: "sha".into(),
            }
            .pr_number(),
            99,
        );
    }

    #[test]
    fn artifact_ref_round_trips_through_serde() {
        let r = GitHubArtifactRef {
            branch: "feat/mirror".into(),
            pr_number: 123,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: GitHubArtifactRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
