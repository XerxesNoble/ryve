// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Delegation trace — visualizes the Atlas → Head → Hand chain that led
//! to a given spark being worked on.
//!
//! Atlas is the conceptual Director: Ryve's primary user-facing agent
//! (sp ryve-5472d4c6). The trace always renders Atlas as the fixed root
//! and threads from there into whichever Head spawned the Hand currently
//! assigned to the spark, and the Hands themselves. Atlas may also have
//! a persisted `agent_sessions` row (`session_label = "atlas"`), but the
//! trace does not depend on that row — Atlas is rendered structurally as
//! the Director regardless.
//!
//! This module is split into a pure-data builder (`build_trace`) and a
//! view (`view`). Keeping the build step pure lets us unit-test the
//! delegation graph without spinning up Iced.

use data::sparks::types::{Crew, CrewMember, HandAssignment, Spark};
use iced::widget::{Space, column, container, row, text};
use iced::{Element, Length, Theme};

use crate::screen::agents::AgentSession;
use crate::screen::spark_detail::Message;
use crate::style::{FONT_BODY, FONT_LABEL, FONT_SMALL, Palette};

// ── Pure data ────────────────────────────────────────

/// Lightweight, owned snapshot of the delegation chain for one spark.
/// `head` is `None` when the spark has no Hand assigned, or when the
/// assigned Hand has no resolvable parent Head (a true standalone).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DelegationTrace {
    /// Head node, if any. Renders between Atlas and Hand.
    pub head: Option<HeadTraceNode>,
    /// Hands assigned to this spark, sorted by assignment `id` order.
    pub hands: Vec<HandTraceNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadTraceNode {
    pub session_id: String,
    /// Display name pulled from the AgentSession, falling back to the id.
    pub display_name: String,
    /// Crew this Head owns that's responsible for the spark, if any.
    pub crew_id: Option<String>,
    pub crew_name: Option<String>,
    /// Parent epic spark id (`crews.parent_spark_id`), if any.
    pub epic_spark_id: Option<String>,
    /// Title of the parent epic, if it can be resolved from `sparks`.
    pub epic_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandTraceNode {
    pub session_id: String,
    /// Display name pulled from the AgentSession, falling back to the id.
    pub display_name: String,
    /// `hand_assignments.role` (owner / assistant / observer / merger).
    pub role: String,
    /// `hand_assignments.status` (active / completed / handed_off / ...).
    pub status: String,
    pub assigned_at: String,
}

/// Build the delegation trace for `spark_id` from the workshop's cached
/// tables. Pure function — no I/O, no Iced — so it can be unit-tested.
///
/// Resolution order for the Head:
/// 1. If any Hand assigned to this spark is a member of a Crew, the
///    Head of that Crew is used (its `head_session_id`).
/// 2. Otherwise, if the Hand session has a `parent_session_id` that
///    matches a known `head_session_id`, that Head is used.
/// 3. Otherwise the Hand is treated as standalone — `head` is `None`.
///
/// The first resolved Head wins; we don't try to render a multi-Head
/// trace because the workgraph today never assigns more than one Crew
/// per spark.
pub fn build_trace(
    spark_id: &str,
    assignments: &[HandAssignment],
    sessions: &[AgentSession],
    crews: &[Crew],
    crew_members: &[CrewMember],
    sparks: &[Spark],
) -> DelegationTrace {
    let session_lookup = |sid: &str| sessions.iter().find(|s| s.id == sid);
    let crew_lookup = |cid: &str| crews.iter().find(|c| c.id == cid);
    let spark_lookup = |sid: &str| sparks.iter().find(|s| s.id == sid);

    // Set of session_ids known to be Heads (own at least one crew).
    let head_session_ids: std::collections::HashSet<&str> = crews
        .iter()
        .filter_map(|c| c.head_session_id.as_deref())
        .collect();

    // Hands assigned to this spark, in stable id order.
    let mut spark_assignments: Vec<&HandAssignment> = assignments
        .iter()
        .filter(|a| a.spark_id == spark_id)
        .collect();
    spark_assignments.sort_by_key(|a| a.id);

    let mut hands = Vec::with_capacity(spark_assignments.len());
    let mut resolved_head: Option<HeadTraceNode> = None;

    for a in &spark_assignments {
        let session = session_lookup(&a.session_id);
        let display_name = session
            .map(|s| s.name.clone())
            .unwrap_or_else(|| a.session_id.clone());

        hands.push(HandTraceNode {
            session_id: a.session_id.clone(),
            display_name,
            role: a.role.clone(),
            status: a.status.clone(),
            assigned_at: a.assigned_at.clone(),
        });

        if resolved_head.is_some() {
            continue;
        }

        // (1) Crew membership: find any crew the Hand belongs to that
        //     itself has a Head session id, then look up the Head.
        let crew_via_membership = crew_members
            .iter()
            .filter(|m| m.session_id == a.session_id)
            .find_map(|m| {
                let crew = crew_lookup(&m.crew_id)?;
                let head_id = crew.head_session_id.as_deref()?;
                Some((crew, head_id.to_string()))
            });

        if let Some((crew, head_id)) = crew_via_membership {
            resolved_head = Some(make_head_node(
                &head_id,
                Some(crew),
                session_lookup,
                spark_lookup,
            ));
            continue;
        }

        // (2) parent_session_id fallback for solo Hands dispatched by a
        //     Head outside of any Crew.
        if let Some(parent_id) = session
            .and_then(|s| s.parent_session_id.as_deref())
            .filter(|pid| head_session_ids.contains(pid))
        {
            // Try to attach a representative Crew owned by that Head, if
            // one happens to point at the spark or its parent — purely
            // informational.
            let crew = crews
                .iter()
                .find(|c| c.head_session_id.as_deref() == Some(parent_id));
            resolved_head = Some(make_head_node(
                parent_id,
                crew,
                session_lookup,
                spark_lookup,
            ));
        }
    }

    DelegationTrace {
        head: resolved_head,
        hands,
    }
}

fn make_head_node<'a, F, G>(
    head_session_id: &str,
    crew: Option<&'a Crew>,
    session_lookup: F,
    spark_lookup: G,
) -> HeadTraceNode
where
    F: Fn(&str) -> Option<&'a AgentSession>,
    G: Fn(&str) -> Option<&'a Spark>,
{
    let session = session_lookup(head_session_id);
    let display_name = session
        .map(|s| s.name.clone())
        .unwrap_or_else(|| head_session_id.to_string());

    let (crew_id, crew_name, epic_spark_id, epic_title) = match crew {
        Some(c) => {
            let epic_id = c.parent_spark_id.clone();
            let epic_title = epic_id
                .as_deref()
                .and_then(spark_lookup)
                .map(|s| s.title.clone());
            (
                Some(c.id.clone()),
                Some(c.name.clone()),
                epic_id,
                epic_title,
            )
        }
        None => (None, None, None, None),
    };

    HeadTraceNode {
        session_id: head_session_id.to_string(),
        display_name,
        crew_id,
        crew_name,
        epic_spark_id,
        epic_title,
    }
}

// ── View ─────────────────────────────────────────────

/// Render the trace as an Atlas → Head → Hand chain. Always rendered, even
/// when no Hand is assigned, so the Director hierarchy is always visible.
pub fn view(trace: &DelegationTrace, pal: &Palette) -> Element<'static, Message> {
    let pal = *pal;

    let header = text("Delegation Trace")
        .size(FONT_LABEL)
        .color(pal.text_tertiary);

    // Atlas (always present, conceptual Director).
    let atlas_node = node_card(
        "Director",
        "Atlas".to_string(),
        Some("primary user-facing agent".to_string()),
        pal.accent,
        &pal,
    );

    let mut chain = column![atlas_node].spacing(2);

    // Arrow → Head
    if let Some(ref head) = trace.head {
        chain = chain.push(arrow_row(&pal));
        let subtitle = match (&head.crew_name, &head.epic_spark_id) {
            (Some(name), Some(epic)) => Some(format!("crew {name} · {epic}")),
            (Some(name), None) => Some(format!("crew {name}")),
            (None, Some(epic)) => Some(format!("epic {epic}")),
            (None, None) => None,
        };
        chain = chain.push(node_card(
            "Head",
            head.display_name.clone(),
            subtitle,
            pal.text_primary,
            &pal,
        ));
    }

    // Arrow → Hand(s)
    if trace.hands.is_empty() {
        chain = chain.push(arrow_row(&pal));
        chain = chain.push(node_card(
            "Hand",
            "(unassigned)".to_string(),
            Some("no Hand has claimed this spark".to_string()),
            pal.text_tertiary,
            &pal,
        ));
    } else {
        for hand in &trace.hands {
            chain = chain.push(arrow_row(&pal));
            let subtitle = format!("{} · {}", hand.role, hand.status);
            let color = hand_color(&hand.status, &pal);
            chain = chain.push(node_card(
                "Hand",
                hand.display_name.clone(),
                Some(subtitle),
                color,
                &pal,
            ));
        }
    }

    column![header, chain].spacing(4).into()
}

fn arrow_row(pal: &Palette) -> Element<'static, Message> {
    let pal = *pal;
    container(
        text("\u{2193}") // ↓
            .size(FONT_SMALL)
            .color(pal.text_tertiary),
    )
    .padding([0, 12])
    .into()
}

fn node_card(
    role_label: &'static str,
    title: String,
    subtitle: Option<String>,
    accent: iced::Color,
    pal: &Palette,
) -> Element<'static, Message> {
    let pal = *pal;
    let role_pill = container(text(role_label).size(FONT_SMALL).color(accent)).padding([1, 6]);
    let title_text = text(title).size(FONT_BODY).color(pal.text_primary);

    let mut col = column![
        row![role_pill, title_text, Space::new().width(Length::Fill),]
            .spacing(6)
            .align_y(iced::Alignment::Center)
    ]
    .spacing(2);

    if let Some(sub) = subtitle {
        col = col
            .push(container(text(sub).size(FONT_SMALL).color(pal.text_tertiary)).padding([0, 6]));
    }

    container(col)
        .padding([4, 8])
        .width(Length::Fill)
        .style(move |_t: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(iced::Color {
                a: 0.05,
                ..pal.text_primary
            })),
            border: iced::Border {
                radius: 4.0.into(),
                width: 1.0,
                color: iced::Color { a: 0.25, ..accent },
            },
            ..Default::default()
        })
        .into()
}

fn hand_color(status: &str, pal: &Palette) -> iced::Color {
    match status {
        "active" => pal.accent,
        "completed" => pal.text_secondary,
        "handed_off" => pal.text_tertiary,
        "abandoned" | "expired" => pal.danger,
        _ => pal.text_secondary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_agents::{CodingAgent, CompatStatus, ResumeStrategy};

    fn test_agent() -> CodingAgent {
        CodingAgent {
            display_name: "Claude".to_string(),
            command: "claude".to_string(),
            args: vec![],
            resume: ResumeStrategy::None,
            compatibility: CompatStatus::Unknown,
        }
    }

    fn make_session(id: &str, name: &str, parent: Option<&str>) -> AgentSession {
        AgentSession {
            id: id.to_string(),
            name: name.to_string(),
            agent: test_agent(),
            tab_id: None,
            active: true,
            stale: false,
            resume_id: None,
            started_at: "2026-04-08T00:00:00Z".to_string(),
            log_path: None,
            last_output_at: None,
            parent_session_id: parent.map(|s| s.to_string()),
            session_label: None,
            tmux_session_live: false,
        }
    }

    fn make_assignment(id: i64, session_id: &str, spark_id: &str) -> HandAssignment {
        HandAssignment {
            id,
            session_id: session_id.to_string(),
            spark_id: spark_id.to_string(),
            status: "active".to_string(),
            role: "owner".to_string(),
            assigned_at: "2026-04-08T00:00:00Z".to_string(),
            last_heartbeat_at: None,
            lease_expires_at: None,
            completed_at: None,
            handoff_to: None,
            handoff_reason: None,
        }
    }

    fn make_crew(id: &str, head: Option<&str>, parent_spark: Option<&str>) -> Crew {
        Crew {
            id: id.to_string(),
            workshop_id: "ws".to_string(),
            name: format!("crew-{id}"),
            purpose: None,
            status: "active".to_string(),
            head_session_id: head.map(String::from),
            parent_spark_id: parent_spark.map(String::from),
            created_at: "2026-04-08T00:00:00Z".to_string(),
        }
    }

    fn make_member(id: i64, crew: &str, session: &str) -> CrewMember {
        CrewMember {
            id,
            crew_id: crew.to_string(),
            session_id: session.to_string(),
            role: None,
            joined_at: "2026-04-08T00:00:00Z".to_string(),
        }
    }

    fn make_spark(id: &str, title: &str) -> Spark {
        Spark {
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            status: "open".to_string(),
            priority: 2,
            spark_type: "task".to_string(),
            assignee: None,
            owner: None,
            parent_id: None,
            workshop_id: "ws".to_string(),
            estimated_minutes: None,
            github_issue_number: None,
            github_repo: None,
            metadata: "{}".to_string(),
            created_at: "2026-04-08T00:00:00Z".to_string(),
            updated_at: "2026-04-08T00:00:00Z".to_string(),
            closed_at: None,
            closed_reason: None,
            due_at: None,
            defer_until: None,
            risk_level: None,
            scope_boundary: None,
        }
    }

    #[test]
    fn unassigned_spark_has_no_head_no_hands() {
        let trace = build_trace("sp-x", &[], &[], &[], &[], &[]);
        assert!(trace.head.is_none());
        assert!(trace.hands.is_empty());
    }

    #[test]
    fn standalone_hand_has_no_head() {
        let sessions = vec![make_session("hand-1", "Claude", None)];
        let assignments = vec![make_assignment(1, "hand-1", "sp-x")];
        let trace = build_trace("sp-x", &assignments, &sessions, &[], &[], &[]);
        assert!(trace.head.is_none());
        assert_eq!(trace.hands.len(), 1);
        assert_eq!(trace.hands[0].display_name, "Claude");
        assert_eq!(trace.hands[0].role, "owner");
    }

    #[test]
    fn crew_member_resolves_head_via_crew() {
        let sessions = vec![
            make_session("head-1", "Head Claude", None),
            make_session("hand-1", "Worker Claude", None),
        ];
        let crews = vec![make_crew("crew-A", Some("head-1"), Some("sp-epic"))];
        let members = vec![make_member(1, "crew-A", "hand-1")];
        let assignments = vec![make_assignment(1, "hand-1", "sp-x")];
        let sparks = vec![make_spark("sp-epic", "The Epic")];

        let trace = build_trace("sp-x", &assignments, &sessions, &crews, &members, &sparks);
        let head = trace.head.expect("head should resolve via crew");
        assert_eq!(head.session_id, "head-1");
        assert_eq!(head.display_name, "Head Claude");
        assert_eq!(head.crew_id.as_deref(), Some("crew-A"));
        assert_eq!(head.epic_spark_id.as_deref(), Some("sp-epic"));
        assert_eq!(head.epic_title.as_deref(), Some("The Epic"));
        assert_eq!(trace.hands.len(), 1);
    }

    #[test]
    fn solo_hand_resolves_head_via_parent_session_id() {
        let sessions = vec![
            make_session("head-1", "Head", None),
            // Hand is parented to a known Head but not in any crew.
            make_session("hand-1", "Solo", Some("head-1")),
        ];
        // Head must own at least one crew to be recognized as a Head.
        let crews = vec![make_crew("crew-A", Some("head-1"), None)];
        let assignments = vec![make_assignment(1, "hand-1", "sp-x")];

        let trace = build_trace("sp-x", &assignments, &sessions, &crews, &[], &[]);
        let head = trace
            .head
            .expect("head should resolve via parent_session_id");
        assert_eq!(head.session_id, "head-1");
    }

    #[test]
    fn parent_pointing_at_non_head_falls_through_to_standalone() {
        let sessions = vec![
            // Parent is not a Head (owns no crews).
            make_session("hand-0", "Sibling", None),
            make_session("hand-1", "Worker", Some("hand-0")),
        ];
        let assignments = vec![make_assignment(1, "hand-1", "sp-x")];
        let trace = build_trace("sp-x", &assignments, &sessions, &[], &[], &[]);
        assert!(trace.head.is_none());
        assert_eq!(trace.hands.len(), 1);
    }

    #[test]
    fn assignments_for_other_sparks_are_ignored() {
        let sessions = vec![make_session("hand-1", "Mine", None)];
        let assignments = vec![
            make_assignment(1, "hand-1", "sp-other"),
            make_assignment(2, "hand-1", "sp-x"),
        ];
        let trace = build_trace("sp-x", &assignments, &sessions, &[], &[], &[]);
        assert_eq!(trace.hands.len(), 1);
        assert_eq!(trace.hands[0].session_id, "hand-1");
    }

    #[test]
    fn unknown_session_falls_back_to_id_for_display_name() {
        let assignments = vec![make_assignment(1, "ghost", "sp-x")];
        let trace = build_trace("sp-x", &assignments, &[], &[], &[], &[]);
        assert_eq!(trace.hands[0].display_name, "ghost");
    }
}
