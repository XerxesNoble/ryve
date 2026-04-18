// SPDX-License-Identifier: AGPL-3.0-or-later

//! Golden Rule lint: every event type on the signal-discipline allow-list
//! MUST have a corresponding `event_to_irc` renderer arm.
//!
//! Without this gate, a new event type could land on the allow-list with
//! no renderer — silently breaking the "no invisible work" contract by
//! passing the gatekeeper and then failing silently at render time. This
//! integration test closes that loop:
//!
//! - [`v1_event_types_all_pass_signal_discipline`] asserts every entry in
//!   [`irc_renderer::V1_EVENT_TYPES`] is `is_allowed`.
//! - [`signal_discipline_exact_allow_list_is_covered_by_renderer`] walks
//!   the allow-list from [`signal_discipline::EXACT_ALLOW`] and fails if
//!   any entry is missing from `V1_EVENT_TYPES`.
//! - [`every_allow_listed_type_renders_to_some`] iterates
//!   `V1_EVENT_TYPES`, builds a synthetic event via `synthetic_payload`,
//!   and asserts `event_to_irc` returns `Some`.
//! - [`off_allow_list_types_never_render`] asserts heartbeat and unknown
//!   strings return `false` from `is_allowed` and `None` from
//!   `synthetic_payload` — so they can never reach `event_to_irc`.

use ipc::irc_renderer::{
    EpicRef, IrcCommand, OutboxEvent, V1_EVENT_TYPES, event_to_irc, synthetic_payload,
};
use ipc::signal_discipline::{ALWAYS_DENY, EXACT_ALLOW, PREFIX_ALLOW, is_allowed};

fn epic() -> EpicRef {
    EpicRef {
        id: "1".into(),
        name: "golden-rule".into(),
    }
}

fn build_event(event_type: &str) -> OutboxEvent {
    let payload = synthetic_payload(event_type)
        .unwrap_or_else(|| panic!("synthetic_payload missing for {event_type}"));
    OutboxEvent {
        event_id: format!("evt-{event_type}"),
        epic: epic(),
        payload,
    }
}

#[test]
fn v1_event_types_all_pass_signal_discipline() {
    for ty in V1_EVENT_TYPES {
        assert!(
            is_allowed(ty),
            "renderer's V1_EVENT_TYPES entry {ty} is not on the signal-discipline allow-list"
        );
    }
}

#[test]
fn signal_discipline_exact_allow_list_is_covered_by_renderer() {
    for ty in EXACT_ALLOW {
        assert!(
            V1_EVENT_TYPES.contains(ty),
            "signal-discipline EXACT_ALLOW entry {ty} has no renderer arm — add a variant to \
             irc_renderer::EventPayload and an entry to V1_EVENT_TYPES"
        );
        assert!(
            synthetic_payload(ty).is_some(),
            "synthetic_payload missing arm for exact-allowed type {ty}"
        );
    }
}

#[test]
fn every_allow_listed_type_renders_to_some() {
    for ty in V1_EVENT_TYPES {
        let event = build_event(ty);
        let line = event_to_irc(&event)
            .unwrap_or_else(|| panic!("event_to_irc returned None for allow-listed type {ty}"));
        assert_eq!(line.command, IrcCommand::Privmsg);
        let structured = line
            .structured
            .as_ref()
            .unwrap_or_else(|| panic!("missing structured attachment for {ty}"));
        assert_eq!(
            structured.event_type, *ty,
            "rendered event_type disagrees with allow-list string"
        );
    }
}

#[test]
fn every_prefix_allowed_subtype_in_v1_is_covered() {
    // Prefix allow means any `github.pr.*` suffix passes signal discipline.
    // The renderer cannot cover infinite suffixes; instead we assert that
    // every v1 subtype we claim to support is (a) prefix-matched by
    // signal-discipline and (b) present in V1_EVENT_TYPES.
    let v1_github_subtypes = V1_EVENT_TYPES
        .iter()
        .filter(|t| PREFIX_ALLOW.iter().any(|p| t.starts_with(p)))
        .copied()
        .collect::<Vec<_>>();
    assert!(
        !v1_github_subtypes.is_empty(),
        "expected at least one github.pr.* subtype in V1_EVENT_TYPES"
    );
    for ty in v1_github_subtypes {
        assert!(is_allowed(ty), "{ty} should match a PREFIX_ALLOW entry");
    }
}

#[test]
fn off_allow_list_types_never_render() {
    // Heartbeats are pinned to ALWAYS_DENY — regardless of future
    // allow-list growth, they must be filtered before ever reaching the
    // renderer. `synthetic_payload` has no variant for them, so even a
    // deliberate attempt to render heartbeats at the type level fails.
    let off_list: &[&str] = &[
        "assignment.heartbeat",
        "unknown",
        "",
        "assignment.unknown",
        "github.issue.opened",
        "random.event.type",
    ];
    for ty in off_list {
        assert!(
            !is_allowed(ty),
            "{ty} must be filtered by signal discipline"
        );
        assert!(
            synthetic_payload(ty).is_none(),
            "synthetic_payload should be None for off-allow-list type {ty}; a new renderer arm \
             without an allow-list entry would silently break the Golden Rule"
        );
    }

    // Defence-in-depth: the pinned deny-list is enforced.
    for ty in ALWAYS_DENY {
        assert!(!is_allowed(ty), "ALWAYS_DENY entry {ty} must stay denied");
        assert!(
            synthetic_payload(ty).is_none(),
            "ALWAYS_DENY entry {ty} must not have a renderer"
        );
    }
}

#[test]
fn renderer_count_matches_v1_contract() {
    // Epic ryve-ddf6fd7f pins v1 at exactly 14 allow-listed event types.
    // Changing this requires updating the epic contract, not silently
    // growing the list.
    assert_eq!(
        V1_EVENT_TYPES.len(),
        14,
        "v1 contract is exactly 14 event types; update the epic before changing this"
    );
}
