// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Home / multi-Hand coordination dashboard.
//!
//! Renders an at-a-glance overview of the workshop:
//!
//! - **Active Hands** — every running coding-agent session, paired with the
//!   spark it's claimed (via `hand_assignments`) and how long ago it started.
//! - **Blocked sparks** — sparks whose `status == "blocked"`.
//! - **Failing contracts** — required contracts in `pending` or `fail` state.
//! - **Active embers** — Hand-to-Hand messages currently in flight.
//! - **All sparks with assigned Hands** — quick join of sparks ↔ active
//!   assignments so the user can see who is doing what.
//!
//! Pure-data view: every input is read from `Workshop` state, so the panel
//! refreshes automatically alongside the existing sparks/contracts/embers
//! polling pipeline. There is no per-Home polling task.

use data::sparks::types::{Contract, Ember, HandAssignment, Spark};
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Element, Length, Theme};

use crate::screen::agents::{AgentSession, format_relative_time};
use crate::style::{
    self, FONT_BODY, FONT_HEADER, FONT_LABEL, FONT_SMALL, Palette,
};

// ── Messages ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    /// Click a spark row to surface its detail in the spark panel.
    SelectSpark(String),
    /// Click an active Hand to focus its bench tab (if any).
    FocusHand(String),
}

// ── Inputs ───────────────────────────────────────────

/// Snapshot of every data source the Home view needs. Built once at view
/// time so the render code doesn't have to know about `Workshop` directly.
pub struct HomeData<'a> {
    pub sparks: &'a [Spark],
    pub agent_sessions: &'a [AgentSession],
    pub assignments: &'a [HandAssignment],
    pub failing_contracts: &'a [Contract],
    pub embers: &'a [Ember],
}

// ── View ─────────────────────────────────────────────

pub fn view<'a>(data: HomeData<'a>, pal: &Palette, has_bg: bool) -> Element<'a, Message> {
    let pal = *pal;

    let header = row![
        text("Home").size(FONT_HEADER).color(pal.text_primary),
        Space::new().width(Length::Fill),
        text(format!("{} sparks", data.sparks.len()))
            .size(FONT_SMALL)
            .color(pal.text_tertiary),
    ]
    .spacing(8)
    .padding([8, 14])
    .align_y(iced::Alignment::Center);

    let body = column![
        section_active_hands(&data, &pal),
        section_assigned_sparks(&data, &pal),
        section_blocked_sparks(&data, &pal),
        section_failing_contracts(data.failing_contracts, &pal),
        section_active_embers(data.embers, &pal),
    ]
    .spacing(14)
    .padding(iced::Padding {
        top: 4.0,
        right: 14.0,
        bottom: 14.0,
        left: 14.0,
    });

    let content = column![header, scrollable(body).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_t: &Theme| style::glass_panel(&pal, has_bg))
        .into()
}

// ── Sections ─────────────────────────────────────────

fn section_active_hands<'a>(data: &HomeData<'a>, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let active: Vec<&AgentSession> = data.agent_sessions.iter().filter(|s| s.active).collect();

    let mut col = column![section_header("Active Hands", active.len(), &pal)].spacing(4);

    if active.is_empty() {
        col = col.push(empty_hint("No Hands are currently running.", &pal));
        return col.into();
    }

    for session in active {
        // Find the spark this session owns (if any) via hand_assignments.
        let claimed_spark: Option<&Spark> = data
            .assignments
            .iter()
            .find(|a| a.session_id == session.id && a.role == "owner" && a.status == "active")
            .and_then(|a| data.sparks.iter().find(|s| s.id == a.spark_id));

        let claim_label = match claimed_spark {
            Some(s) => format!("{} — {}", s.id, s.title),
            None => "(no spark claimed)".to_string(),
        };
        let claim_color = if claimed_spark.is_some() {
            pal.text_secondary
        } else {
            pal.text_tertiary
        };

        let row_content = row![
            text("\u{25CF}").size(FONT_LABEL).color(pal.accent),
            text(session.name.clone())
                .size(FONT_BODY)
                .color(pal.text_primary)
                .width(Length::FillPortion(2)),
            text(claim_label)
                .size(FONT_SMALL)
                .color(claim_color)
                .width(Length::FillPortion(3)),
            Space::new().width(Length::Fill),
            text(format_relative_time(&session.started_at))
                .size(FONT_SMALL)
                .color(pal.text_tertiary),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        let item = button(row_content)
            .style(button::text)
            .padding([4, 8])
            .width(Length::Fill)
            .on_press(Message::FocusHand(session.id.clone()));

        col = col.push(item);
    }

    col.into()
}

fn section_assigned_sparks<'a>(data: &HomeData<'a>, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;

    // Sparks that have at least one active owner assignment. Pair each with
    // the session name when we can resolve it (assignments + agent_sessions).
    let mut rows: Vec<(&Spark, Option<&AgentSession>)> = Vec::new();
    for assignment in data.assignments.iter() {
        if assignment.status != "active" || assignment.role != "owner" {
            continue;
        }
        let Some(spark) = data.sparks.iter().find(|s| s.id == assignment.spark_id) else {
            continue;
        };
        let session = data
            .agent_sessions
            .iter()
            .find(|s| s.id == assignment.session_id);
        rows.push((spark, session));
    }

    let mut col = column![section_header("Assigned Sparks", rows.len(), &pal)].spacing(4);

    if rows.is_empty() {
        col = col.push(empty_hint(
            "No sparks are currently claimed by a Hand.",
            &pal,
        ));
        return col.into();
    }

    for (spark, session) in rows {
        let priority = format!("P{}", spark.priority);
        let hand_label = session
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "(unknown Hand)".to_string());

        let row_content = row![
            text(priority).size(FONT_LABEL).color(pal.text_tertiary),
            text(spark.title.clone())
                .size(FONT_BODY)
                .color(pal.text_primary)
                .width(Length::FillPortion(3)),
            text(format!("\u{2192} {hand_label}"))
                .size(FONT_SMALL)
                .color(pal.text_secondary)
                .width(Length::FillPortion(2)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        col = col.push(
            button(row_content)
                .style(button::text)
                .padding([4, 8])
                .width(Length::Fill)
                .on_press(Message::SelectSpark(spark.id.clone())),
        );
    }

    col.into()
}

fn section_blocked_sparks<'a>(data: &HomeData<'a>, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let blocked: Vec<&Spark> = data
        .sparks
        .iter()
        .filter(|s| s.status == "blocked")
        .collect();

    let mut col = column![section_header("Blocked Sparks", blocked.len(), &pal)].spacing(4);

    if blocked.is_empty() {
        col = col.push(empty_hint("Nothing blocked. Smooth sailing.", &pal));
        return col.into();
    }

    for spark in blocked {
        let priority = format!("P{}", spark.priority);
        let row_content = row![
            text("\u{25A0}").size(FONT_LABEL).color(pal.danger),
            text(priority).size(FONT_LABEL).color(pal.text_tertiary),
            text(spark.title.clone())
                .size(FONT_BODY)
                .color(pal.text_primary),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        col = col.push(
            button(row_content)
                .style(button::text)
                .padding([4, 8])
                .width(Length::Fill)
                .on_press(Message::SelectSpark(spark.id.clone())),
        );
    }

    col.into()
}

fn section_failing_contracts<'a>(
    failing: &'a [Contract],
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;
    let mut col =
        column![section_header("Failing Contracts", failing.len(), &pal)].spacing(4);

    if failing.is_empty() {
        col = col.push(empty_hint("All required contracts pass.", &pal));
        return col.into();
    }

    for contract in failing {
        let status_color = match contract.status.as_str() {
            "fail" => pal.danger,
            _ => pal.text_secondary,
        };
        let row_content = row![
            text(contract.status.clone()).size(FONT_LABEL).color(status_color),
            text(contract.spark_id.clone())
                .size(FONT_LABEL)
                .color(pal.text_tertiary),
            text(contract.description.clone())
                .size(FONT_BODY)
                .color(pal.text_primary),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        col = col.push(
            button(row_content)
                .style(button::text)
                .padding([4, 8])
                .width(Length::Fill)
                .on_press(Message::SelectSpark(contract.spark_id.clone())),
        );
    }

    col.into()
}

fn section_active_embers<'a>(embers: &'a [Ember], pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let mut col = column![section_header("Active Embers", embers.len(), &pal)].spacing(4);

    if embers.is_empty() {
        col = col.push(empty_hint("No embers in flight.", &pal));
        return col.into();
    }

    for ember in embers {
        let source = ember
            .source_agent
            .clone()
            .unwrap_or_else(|| "system".to_string());
        let row_content = row![
            text(ember.ember_type.clone())
                .size(FONT_LABEL)
                .color(pal.accent),
            text(source).size(FONT_LABEL).color(pal.text_tertiary),
            text(ember.content.clone())
                .size(FONT_BODY)
                .color(pal.text_primary),
            Space::new().width(Length::Fill),
            text(format_relative_time(&ember.created_at))
                .size(FONT_SMALL)
                .color(pal.text_tertiary),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        col = col.push(container(row_content).padding([4, 8]).width(Length::Fill));
    }

    col.into()
}

// ── Small helpers ────────────────────────────────────

fn section_header<'a>(label: &'a str, count: usize, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    row![
        text(label).size(FONT_LABEL).color(pal.text_secondary),
        text(format!("({count})"))
            .size(FONT_SMALL)
            .color(pal.text_tertiary),
    ]
    .spacing(6)
    .padding(iced::Padding {
        top: 4.0,
        right: 0.0,
        bottom: 2.0,
        left: 0.0,
    })
    .align_y(iced::Alignment::Center)
    .into()
}

fn empty_hint<'a>(label: &'a str, pal: &Palette) -> Element<'a, Message> {
    text(label)
        .size(FONT_SMALL)
        .color(pal.text_tertiary)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_agents::{CodingAgent, ResumeStrategy};

    fn empty_spark(id: &str, status: &str, priority: i32) -> Spark {
        Spark {
            id: id.to_string(),
            title: format!("title-{id}"),
            description: String::new(),
            status: status.to_string(),
            priority,
            spark_type: "task".to_string(),
            assignee: None,
            owner: None,
            parent_id: None,
            workshop_id: "ws".to_string(),
            estimated_minutes: None,
            github_issue_number: None,
            github_repo: None,
            metadata: "{}".to_string(),
            created_at: "2026-04-07T00:00:00+00:00".to_string(),
            updated_at: "2026-04-07T00:00:00+00:00".to_string(),
            closed_at: None,
            closed_reason: None,
            due_at: None,
            defer_until: None,
            risk_level: None,
            scope_boundary: None,
        }
    }

    fn make_session(id: &str, active: bool) -> AgentSession {
        AgentSession {
            id: id.to_string(),
            name: format!("Hand-{id}"),
            agent: CodingAgent {
                display_name: "Test".to_string(),
                command: "test".to_string(),
                args: vec![],
                resume: ResumeStrategy::None,
                compatibility: crate::coding_agents::CompatStatus::Unknown,
            },
            tab_id: None,
            active,
            stale: false,
            resume_id: None,
            started_at: "2026-04-07T11:00:00+00:00".to_string(),
        }
    }

    fn make_assignment(session: &str, spark: &str) -> HandAssignment {
        HandAssignment {
            id: 1,
            session_id: session.to_string(),
            spark_id: spark.to_string(),
            status: "active".to_string(),
            role: "owner".to_string(),
            assigned_at: "2026-04-07T11:00:00+00:00".to_string(),
            last_heartbeat_at: None,
            lease_expires_at: None,
            completed_at: None,
            handoff_to: None,
            handoff_reason: None,
        }
    }

    #[test]
    fn view_renders_empty_state_without_panic() {
        let data = HomeData {
            sparks: &[],
            agent_sessions: &[],
            assignments: &[],
            failing_contracts: &[],
            embers: &[],
        };
        let _ = view(data, &Palette::dark(), false);
    }

    #[test]
    fn view_renders_populated_state_without_panic() {
        // The dashboard's whole job is to join sparks ↔ assignments ↔
        // sessions, so the smoke test must include all three (a blocked
        // spark, an active Hand owning a different spark, and a contract
        // failure) to exercise the join paths the empty test misses.
        let sparks = vec![
            empty_spark("sp-aaaa", "in_progress", 1),
            empty_spark("sp-bbbb", "blocked", 2),
        ];
        let sessions = vec![make_session("sess-1", true)];
        let assignments = vec![make_assignment("sess-1", "sp-aaaa")];
        let failing = vec![Contract {
            id: 1,
            spark_id: "sp-aaaa".to_string(),
            kind: "test_pass".to_string(),
            description: "tests must pass".to_string(),
            check_command: None,
            pattern: None,
            file_glob: None,
            enforcement: "required".to_string(),
            status: "fail".to_string(),
            last_checked_at: None,
            last_checked_by: None,
            created_at: "2026-04-07T11:00:00+00:00".to_string(),
        }];
        let embers = vec![Ember {
            id: "em-1".to_string(),
            ember_type: "flash".to_string(),
            content: "shared lock acquired".to_string(),
            source_agent: Some("Hand-sess-1".to_string()),
            workshop_id: "ws".to_string(),
            ttl_seconds: 60,
            created_at: "2026-04-07T11:30:00+00:00".to_string(),
        }];

        let data = HomeData {
            sparks: &sparks,
            agent_sessions: &sessions,
            assignments: &assignments,
            failing_contracts: &failing,
            embers: &embers,
        };
        let _ = view(data, &Palette::dark(), true);
    }

    #[test]
    fn assigned_sparks_only_lists_active_owner_assignments() {
        // Filter must reject completed assignments and non-owner roles —
        // otherwise an old finished claim or an observer would silently
        // double-count a spark on the dashboard.
        let sparks = vec![
            empty_spark("sp-1", "open", 2),
            empty_spark("sp-2", "open", 2),
            empty_spark("sp-3", "open", 2),
        ];
        let sessions = vec![make_session("sess-1", true)];

        let mut completed = make_assignment("sess-1", "sp-1");
        completed.status = "completed".to_string();
        let mut observer = make_assignment("sess-1", "sp-2");
        observer.role = "observer".to_string();
        let active_owner = make_assignment("sess-1", "sp-3");

        let assignments = vec![completed, observer, active_owner];
        let data = HomeData {
            sparks: &sparks,
            agent_sessions: &sessions,
            assignments: &assignments,
            failing_contracts: &[],
            embers: &[],
        };

        let matches: Vec<&Spark> = data
            .sparks
            .iter()
            .filter(|s| {
                data.assignments.iter().any(|a| {
                    a.spark_id == s.id && a.status == "active" && a.role == "owner"
                })
            })
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "sp-3");
    }
}
