// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signal discipline: the IRC allow-list gatekeeper.
//!
//! [`is_allowed`] is the single entry point for every lifecycle event that
//! might be relayed to IRC. It returns `true` only for event types on the
//! v1 allow-list and drops everything else — heartbeats, internal
//! bookkeeping, and any future event type that ships without an explicit
//! IRC mapping.
//!
//! The allow-list lives in one static data structure so there is no
//! ambiguity about what reaches IRC. Heartbeats are additionally pinned
//! to the deny-list so they stay excluded even if the allow-list grows.

/// Event types that pass the filter by exact match.
///
/// Exposed so the Golden Rule lint (`ipc/tests/irc_golden_rule.rs`) can
/// iterate the allow-list and assert every entry has a matching
/// [`crate::irc_renderer::event_to_irc`] arm.
pub const EXACT_ALLOW: &[&str] = &[
    "assignment.created",
    "assignment.transitioned",
    "assignment.stuck",
    "review.assigned",
    "review.completed",
    "merge.started",
    "merge.completed",
    "epic.blocker_raised",
];

/// Event-type prefixes that pass the filter. A candidate must have at
/// least one character after the prefix — bare "github.pr." is not an
/// event, it is a namespace.
pub const PREFIX_ALLOW: &[&str] = &["github.pr."];

/// Event types that are always dropped, even if they later appear on the
/// allow-list. Protects the invariant that heartbeats never reach IRC.
pub const ALWAYS_DENY: &[&str] = &["assignment.heartbeat"];

/// Returns `true` when `event_type` is on the v1 IRC allow-list.
///
/// Pure function: `(event_type) -> bool`. No per-channel or per-user
/// context — that belongs to the router, not the gatekeeper.
pub fn is_allowed(event_type: &str) -> bool {
    if ALWAYS_DENY.contains(&event_type) {
        return false;
    }
    if EXACT_ALLOW.contains(&event_type) {
        return true;
    }
    PREFIX_ALLOW
        .iter()
        .any(|p| event_type.len() > p.len() && event_type.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_v1_exact_allow_entry_is_allowed() {
        for event in [
            "assignment.created",
            "assignment.transitioned",
            "assignment.stuck",
            "review.assigned",
            "review.completed",
            "merge.started",
            "merge.completed",
            "epic.blocker_raised",
        ] {
            assert!(is_allowed(event), "expected {event} to be allowed");
        }
    }

    #[test]
    fn github_pr_prefix_matches_any_subtype() {
        for event in [
            "github.pr.opened",
            "github.pr.closed",
            "github.pr.merged",
            "github.pr.review_requested",
            "github.pr.anything_future",
        ] {
            assert!(is_allowed(event), "expected {event} to be allowed");
        }
    }

    #[test]
    fn assignment_heartbeat_is_always_denied() {
        assert!(!is_allowed("assignment.heartbeat"));
    }

    #[test]
    fn unknown_event_types_are_denied() {
        for event in [
            "",
            "unknown",
            "assignment",
            "assignment.",
            "assignment.unknown",
            "review",
            "review.pending",
            "merge",
            "merge.aborted",
            "epic.created",
            "github",
            "github.pr",
            "github.pr.",
            "github.push",
            "github.issue.opened",
            "githubxpr.opened",
            "random.event.type",
        ] {
            assert!(!is_allowed(event), "expected {event:?} to be denied");
        }
    }

    #[test]
    fn prefix_match_requires_non_empty_suffix() {
        assert!(!is_allowed("github.pr."));
        assert!(is_allowed("github.pr.x"));
    }

    #[test]
    fn prefix_match_is_anchored_at_start() {
        assert!(!is_allowed("prefix.github.pr.opened"));
    }

    #[test]
    fn match_is_case_sensitive() {
        assert!(!is_allowed("Assignment.Created"));
        assert!(!is_allowed("GITHUB.PR.OPENED"));
    }
}
