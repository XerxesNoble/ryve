// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure translator from raw GitHub webhook / poll payloads into
//! [`CanonicalGitHubEvent`].
//!
//! Every ingestion path (HTTP webhook delivery, REST polling, manual
//! replay) lands here before the applier sees anything. The translator
//! is the single, deterministic mapping from "whatever shape GitHub
//! ships this week" to the vocabulary the applier's decision table
//! speaks.
//!
//! # Purity
//!
//! [`translate`] touches no external state: no database, no network,
//! no clock, no filesystem, no random. Given the same [`GitHubPayload`]
//! it returns byte-identical output — a property exercised by the
//! determinism test at the bottom of this file.
//!
//! # Unknown input
//!
//! Events that are not part of the applier's vocabulary (a `push`,
//! a `pull_request.labeled`, a `check_run` that is still running, an
//! `issue_comment` on a plain issue rather than a PR, …) return
//! [`TranslateError::Unsupported`]. The ingestion edge is expected to
//! log-and-drop these — they must not panic or be forwarded.
//!
//! # Webhook vs. poll
//!
//! The GitHub webhook delivery splits event identification between the
//! `X-GitHub-Event` HTTP header and the JSON body's `action` field.
//! REST polling reconstructs the same shape by setting `event_type`
//! from the resource being polled. [`GitHubPayload`] carries both so
//! the translator does not need to care which ingestion path produced
//! the value.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::CanonicalGitHubEvent;

/// Raw GitHub event as it arrives at the ingestion edge.
///
/// `event_type` mirrors the `X-GitHub-Event` HTTP header (e.g.
/// `"pull_request"`, `"pull_request_review"`, `"issue_comment"`,
/// `"check_run"`). `body` is the full JSON payload as delivered —
/// parsing is deferred to the translator so the edge can reuse a
/// single deserializer and fixtures can be authored as plain JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubPayload {
    pub event_type: String,
    pub body: Value,
}

impl GitHubPayload {
    /// Convenience constructor used by tests and the ingestion edge.
    pub fn new(event_type: impl Into<String>, body: Value) -> Self {
        Self {
            event_type: event_type.into(),
            body,
        }
    }
}

/// Reasons [`translate`] can refuse a payload.
///
/// The translator is total over *syntactically valid* JSON: any value
/// that can be parsed yields one of these variants rather than a panic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranslateError {
    /// The event_type / action pair is not part of the mirror's
    /// vocabulary. The edge should log-and-drop.
    #[error("unsupported GitHub event: {0}")]
    Unsupported(String),

    /// The event_type is supported but a field the translator needs
    /// was missing. Indicates either a GitHub schema change or a
    /// malformed fixture.
    #[error("missing required field: {0}")]
    MissingField(&'static str),

    /// A field was present but had the wrong shape (e.g. a string
    /// where a number was expected). Distinguished from
    /// [`TranslateError::MissingField`] so the edge can tell a schema
    /// drift from a null.
    #[error("invalid field {field}: {reason}")]
    InvalidField { field: &'static str, reason: String },
}

/// Translate a raw GitHub payload into a [`CanonicalGitHubEvent`].
///
/// See the module docs for the full set of supported `(event_type,
/// action)` pairs and the purity contract. Anything else returns
/// [`TranslateError::Unsupported`] — never a panic.
pub fn translate(payload: &GitHubPayload) -> Result<CanonicalGitHubEvent, TranslateError> {
    match payload.event_type.as_str() {
        "pull_request" => translate_pull_request(&payload.body),
        "pull_request_review" => translate_pull_request_review(&payload.body),
        "issue_comment" => translate_issue_comment(&payload.body),
        "check_run" => translate_check_run(&payload.body),
        other => Err(TranslateError::Unsupported(format!("event_type={other}"))),
    }
}

fn translate_pull_request(body: &Value) -> Result<CanonicalGitHubEvent, TranslateError> {
    let action = required_str(body, "action")?;
    let pr = body
        .get("pull_request")
        .ok_or(TranslateError::MissingField("pull_request"))?;
    let pr_number = required_pr_number(pr)?;

    match action {
        "opened" => {
            let head_branch = required_head_ref(pr)?.to_string();
            Ok(CanonicalGitHubEvent::PrOpened {
                pr_number,
                head_branch,
            })
        }
        "edited" | "synchronize" => {
            let head_branch = required_head_ref(pr)?.to_string();
            Ok(CanonicalGitHubEvent::PrUpdated {
                pr_number,
                head_branch,
            })
        }
        "closed" => {
            let merged = pr
                .get("merged")
                .and_then(Value::as_bool)
                .ok_or(TranslateError::MissingField("pull_request.merged"))?;
            if merged {
                // GitHub may omit merge_commit_sha momentarily for
                // squash/rebase merges between the close event firing
                // and the merge commit's SHA being published. Fall
                // back to an empty string when the field is absent or
                // null — callers should treat that as "SHA not yet
                // known" but the event is still terminal so the
                // Assignment can advance to Merged.
                let merge_commit_sha = pr
                    .get("merge_commit_sha")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Ok(CanonicalGitHubEvent::PrMerged {
                    pr_number,
                    merge_commit_sha,
                })
            } else {
                Ok(CanonicalGitHubEvent::PrClosed { pr_number })
            }
        }
        other => Err(TranslateError::Unsupported(format!(
            "pull_request.action={other}"
        ))),
    }
}

fn translate_pull_request_review(body: &Value) -> Result<CanonicalGitHubEvent, TranslateError> {
    let action = required_str(body, "action")?;
    if action != "submitted" {
        return Err(TranslateError::Unsupported(format!(
            "pull_request_review.action={action}"
        )));
    }

    let review = body
        .get("review")
        .ok_or(TranslateError::MissingField("review"))?;
    let state = review
        .get("state")
        .and_then(Value::as_str)
        .ok_or(TranslateError::MissingField("review.state"))?;
    let reviewer = review
        .get("user")
        .and_then(|u| u.get("login"))
        .and_then(Value::as_str)
        .ok_or(TranslateError::MissingField("review.user.login"))?
        .to_string();

    let pr = body
        .get("pull_request")
        .ok_or(TranslateError::MissingField("pull_request"))?;
    let pr_number = required_pr_number(pr)?;

    // GitHub normalises review state to lowercase in its webhook
    // payload (`approved`, `changes_requested`, `commented`, …).
    // `commented` reviews are audit-only for the applier; drop them
    // as Unsupported so the ingestion edge doesn't forward them.
    match state {
        "approved" => Ok(CanonicalGitHubEvent::ReviewApproved {
            pr_number,
            reviewer,
        }),
        "changes_requested" => Ok(CanonicalGitHubEvent::ReviewChangesRequested {
            pr_number,
            reviewer,
        }),
        other => Err(TranslateError::Unsupported(format!(
            "pull_request_review.state={other}"
        ))),
    }
}

fn translate_issue_comment(body: &Value) -> Result<CanonicalGitHubEvent, TranslateError> {
    let action = required_str(body, "action")?;
    if action != "created" {
        return Err(TranslateError::Unsupported(format!(
            "issue_comment.action={action}"
        )));
    }

    let issue = body
        .get("issue")
        .ok_or(TranslateError::MissingField("issue"))?;

    // GitHub reuses the `issue_comment` event for both plain issues
    // and PRs; the `issue.pull_request` sub-object is the only
    // discriminator. Comments on plain issues are out of scope for
    // the PR mirror.
    if issue.get("pull_request").is_none() {
        return Err(TranslateError::Unsupported(
            "issue_comment on non-PR issue".into(),
        ));
    }

    let pr_number = issue
        .get("number")
        .and_then(Value::as_i64)
        .ok_or(TranslateError::MissingField("issue.number"))?;

    let comment = body
        .get("comment")
        .ok_or(TranslateError::MissingField("comment"))?;
    let author = comment
        .get("user")
        .and_then(|u| u.get("login"))
        .and_then(Value::as_str)
        .ok_or(TranslateError::MissingField("comment.user.login"))?
        .to_string();
    let body_text = comment
        .get("body")
        .and_then(Value::as_str)
        .ok_or(TranslateError::MissingField("comment.body"))?
        .to_string();

    Ok(CanonicalGitHubEvent::PrComment {
        pr_number,
        author,
        body: body_text,
    })
}

fn translate_check_run(body: &Value) -> Result<CanonicalGitHubEvent, TranslateError> {
    let action = required_str(body, "action")?;
    if action != "completed" {
        return Err(TranslateError::Unsupported(format!(
            "check_run.action={action}"
        )));
    }

    let check_run = body
        .get("check_run")
        .ok_or(TranslateError::MissingField("check_run"))?;

    let check_name = check_run
        .get("name")
        .and_then(Value::as_str)
        .ok_or(TranslateError::MissingField("check_run.name"))?
        .to_string();

    // GitHub fills in `conclusion` only once the run has finished —
    // which is guaranteed under action="completed" but we still
    // treat a null conclusion as Unsupported rather than fabricating
    // a status the applier would misinterpret. Unsupported lets the
    // caller log-and-drop; MissingField would surface as schema drift.
    let status = check_run
        .get("conclusion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            TranslateError::Unsupported("check_run.conclusion is null (run not finished)".into())
        })?
        .to_string();

    // A check_run may be associated with zero or more PRs; we emit one
    // event per PR match via the caller. For the translator's single
    // input/single output contract, we take the first PR. Events with
    // no associated PRs are Unsupported because the applier has no
    // row to update.
    let prs = check_run
        .get("pull_requests")
        .and_then(Value::as_array)
        .ok_or(TranslateError::MissingField("check_run.pull_requests"))?;
    let first_pr = prs.first().ok_or(TranslateError::Unsupported(
        "check_run with no associated PRs".into(),
    ))?;
    let pr_number =
        first_pr
            .get("number")
            .and_then(Value::as_i64)
            .ok_or(TranslateError::MissingField(
                "check_run.pull_requests[0].number",
            ))?;

    Ok(CanonicalGitHubEvent::CheckRunStatus {
        pr_number,
        check_name,
        status,
    })
}

fn required_str<'a>(value: &'a Value, field: &'static str) -> Result<&'a str, TranslateError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(TranslateError::MissingField(field))
}

fn required_pr_number(pr: &Value) -> Result<i64, TranslateError> {
    pr.get("number")
        .and_then(Value::as_i64)
        .ok_or(TranslateError::MissingField("pull_request.number"))
}

fn required_head_ref(pr: &Value) -> Result<&str, TranslateError> {
    pr.get("head")
        .and_then(|h| h.get("ref"))
        .and_then(Value::as_str)
        .ok_or(TranslateError::MissingField("pull_request.head.ref"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Every fixture committed under `fixtures/github/` together with
    /// the `X-GitHub-Event` header value the HTTP edge would set for it.
    /// Keep this list in sync with the directory — the determinism
    /// test iterates it.
    const FIXTURES: &[(&str, &str)] = &[
        ("pull_request", "pull_request_opened.json"),
        ("pull_request", "pull_request_edited.json"),
        ("pull_request", "pull_request_synchronize.json"),
        ("pull_request", "pull_request_closed_merged.json"),
        ("pull_request", "pull_request_closed_unmerged.json"),
        ("pull_request", "pull_request_reopened.json"),
        ("pull_request_review", "pull_request_review_approved.json"),
        (
            "pull_request_review",
            "pull_request_review_changes_requested.json",
        ),
        ("pull_request_review", "pull_request_review_commented.json"),
        ("issue_comment", "issue_comment_on_pr.json"),
        ("issue_comment", "issue_comment_on_issue.json"),
        ("check_run", "check_run_completed.json"),
        ("check_run", "check_run_completed_failure.json"),
        ("check_run", "check_run_in_progress.json"),
        ("push", "push_event.json"),
    ];

    fn load_fixture(name: &str) -> Value {
        let path = format!("{}/../fixtures/github/{}", env!("CARGO_MANIFEST_DIR"), name);
        let raw =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"));
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse fixture {path}: {e}"))
    }

    fn payload(event_type: &str, fixture: &str) -> GitHubPayload {
        GitHubPayload::new(event_type, load_fixture(fixture))
    }

    #[test]
    fn pull_request_opened_maps_to_pr_opened() {
        let ev = translate(&payload("pull_request", "pull_request_opened.json")).unwrap();
        assert_eq!(
            ev,
            CanonicalGitHubEvent::PrOpened {
                pr_number: 42,
                head_branch: "feat/mirror-translator".into(),
            },
        );
    }

    #[test]
    fn pull_request_edited_maps_to_pr_updated() {
        let ev = translate(&payload("pull_request", "pull_request_edited.json")).unwrap();
        assert_eq!(
            ev,
            CanonicalGitHubEvent::PrUpdated {
                pr_number: 42,
                head_branch: "feat/mirror-translator".into(),
            },
        );
    }

    #[test]
    fn pull_request_synchronize_maps_to_pr_updated() {
        let ev = translate(&payload("pull_request", "pull_request_synchronize.json")).unwrap();
        assert_eq!(
            ev,
            CanonicalGitHubEvent::PrUpdated {
                pr_number: 42,
                head_branch: "feat/mirror-translator".into(),
            },
        );
    }

    #[test]
    fn pull_request_closed_merged_maps_to_pr_merged() {
        let ev = translate(&payload("pull_request", "pull_request_closed_merged.json")).unwrap();
        assert_eq!(
            ev,
            CanonicalGitHubEvent::PrMerged {
                pr_number: 42,
                merge_commit_sha: "deadbeefcafef00d".into(),
            },
        );
    }

    #[test]
    fn pull_request_closed_unmerged_maps_to_pr_closed() {
        let ev = translate(&payload(
            "pull_request",
            "pull_request_closed_unmerged.json",
        ))
        .unwrap();
        assert_eq!(ev, CanonicalGitHubEvent::PrClosed { pr_number: 43 });
    }

    #[test]
    fn pull_request_reopened_is_unsupported() {
        let err = translate(&payload("pull_request", "pull_request_reopened.json")).unwrap_err();
        assert!(
            matches!(err, TranslateError::Unsupported(ref msg) if msg.contains("reopened")),
            "expected Unsupported(reopened), got {err:?}",
        );
    }

    #[test]
    fn review_approved_maps_to_review_approved() {
        let ev = translate(&payload(
            "pull_request_review",
            "pull_request_review_approved.json",
        ))
        .unwrap();
        assert_eq!(
            ev,
            CanonicalGitHubEvent::ReviewApproved {
                pr_number: 42,
                reviewer: "bob".into(),
            },
        );
    }

    #[test]
    fn review_changes_requested_maps_to_review_changes_requested() {
        let ev = translate(&payload(
            "pull_request_review",
            "pull_request_review_changes_requested.json",
        ))
        .unwrap();
        assert_eq!(
            ev,
            CanonicalGitHubEvent::ReviewChangesRequested {
                pr_number: 42,
                reviewer: "carol".into(),
            },
        );
    }

    #[test]
    fn review_commented_is_unsupported() {
        let err = translate(&payload(
            "pull_request_review",
            "pull_request_review_commented.json",
        ))
        .unwrap_err();
        assert!(
            matches!(err, TranslateError::Unsupported(ref msg) if msg.contains("commented")),
            "expected Unsupported(commented), got {err:?}",
        );
    }

    #[test]
    fn issue_comment_on_pr_maps_to_pr_comment() {
        let ev = translate(&payload("issue_comment", "issue_comment_on_pr.json")).unwrap();
        assert_eq!(
            ev,
            CanonicalGitHubEvent::PrComment {
                pr_number: 42,
                author: "erin".into(),
                body: "Looks good, ship it.".into(),
            },
        );
    }

    #[test]
    fn issue_comment_on_plain_issue_is_unsupported() {
        let err = translate(&payload("issue_comment", "issue_comment_on_issue.json")).unwrap_err();
        assert!(
            matches!(err, TranslateError::Unsupported(ref msg) if msg.contains("non-PR")),
            "expected Unsupported(non-PR), got {err:?}",
        );
    }

    #[test]
    fn check_run_completed_success_maps_to_check_run_status() {
        let ev = translate(&payload("check_run", "check_run_completed.json")).unwrap();
        assert_eq!(
            ev,
            CanonicalGitHubEvent::CheckRunStatus {
                pr_number: 42,
                check_name: "ci/build".into(),
                status: "success".into(),
            },
        );
    }

    #[test]
    fn check_run_completed_failure_carries_conclusion() {
        let ev = translate(&payload("check_run", "check_run_completed_failure.json")).unwrap();
        assert_eq!(
            ev,
            CanonicalGitHubEvent::CheckRunStatus {
                pr_number: 42,
                check_name: "ci/test".into(),
                status: "failure".into(),
            },
        );
    }

    #[test]
    fn check_run_in_progress_is_unsupported() {
        let err = translate(&payload("check_run", "check_run_in_progress.json")).unwrap_err();
        assert!(
            matches!(err, TranslateError::Unsupported(ref msg) if msg.contains("action=created")),
            "expected Unsupported(action=created), got {err:?}",
        );
    }

    #[test]
    fn unknown_event_type_is_unsupported() {
        let err = translate(&payload("push", "push_event.json")).unwrap_err();
        assert!(
            matches!(err, TranslateError::Unsupported(ref msg) if msg.contains("event_type=push")),
            "expected Unsupported(event_type=push), got {err:?}",
        );
    }

    #[test]
    fn empty_body_reports_missing_action_not_panic() {
        let err = translate(&GitHubPayload::new("pull_request", json!({}))).unwrap_err();
        assert_eq!(err, TranslateError::MissingField("action"));
    }

    #[test]
    fn missing_head_ref_on_opened_is_missing_field() {
        // Exercises MissingField propagation for a supported action.
        let err = translate(&GitHubPayload::new(
            "pull_request",
            json!({
                "action": "opened",
                "pull_request": { "number": 7 },
            }),
        ))
        .unwrap_err();
        assert_eq!(err, TranslateError::MissingField("pull_request.head.ref"));
    }

    /// Purity guard: `translate(x) == translate(x)` over every
    /// fixture, including the ones that are expected to fail. Any
    /// source of non-determinism (HashMap iteration, clock reads,
    /// RNG, …) introduced by a future refactor will trip this.
    #[test]
    fn translate_is_deterministic_across_fixture_set() {
        for (event_type, fixture) in FIXTURES {
            let p = payload(event_type, fixture);
            let a = translate(&p);
            let b = translate(&p);
            assert_eq!(
                a, b,
                "translate is non-deterministic for ({event_type}, {fixture})",
            );
            // Serialise the Ok case as JSON too — this catches
            // non-determinism that only manifests after serde, e.g.
            // a future HashMap-backed field.
            if let Ok(ev) = a {
                let sa = serde_json::to_string(&ev).unwrap();
                let sb = serde_json::to_string(&translate(&p).unwrap()).unwrap();
                assert_eq!(
                    sa, sb,
                    "serialised output differs across calls for ({event_type}, {fixture})",
                );
            }
        }
    }
}
