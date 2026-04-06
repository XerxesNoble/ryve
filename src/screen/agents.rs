// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Hands panel — lists active and past Hand sessions.

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Element, Length, Theme};
use uuid::Uuid;

use crate::coding_agents::{CodingAgent, ResumeStrategy};
use crate::style::{self, Palette, FONT_BODY, FONT_HEADER, FONT_ICON, FONT_ICON_SM, FONT_LABEL, FONT_SMALL};

#[derive(Debug, Clone)]
pub enum Message {
    /// Switch to an active Hand's tab.
    SelectAgent(Uuid),
    /// Resume a past (ended) Hand session.
    ResumeAgent(String),
    /// Delete a past session from history.
    DeleteSession(String),
}

/// A Hand session shown in the Hands panel.
/// This is the in-memory representation — may or may not have a live terminal.
#[derive(Debug, Clone)]
pub struct AgentSession {
    /// Unique ID (matches the persisted agent_sessions.id).
    pub id: String,
    /// Display name (e.g., "Claude Code").
    pub name: String,
    /// The agent definition (command, args, resume strategy).
    pub agent: CodingAgent,
    /// Tab ID in the bench (Some = currently has a terminal open).
    pub tab_id: Option<u64>,
    /// Whether this session is currently running.
    pub active: bool,
    /// Agent-specific session/conversation ID for resumption.
    pub resume_id: Option<String>,
    /// When the session was started.
    pub started_at: String,
}

impl AgentSession {
    /// Can this session be resumed?
    pub fn can_resume(&self) -> bool {
        !self.active && self.agent.resume != ResumeStrategy::None
    }
}

/// Render the Hands panel.
pub fn view<'a>(sessions: &'a [AgentSession], pal: Palette, has_bg: bool) -> Element<'a, Message> {
    let header = text("Hands").size(FONT_HEADER).color(pal.text_primary);

    let mut content = column![header].spacing(6).padding(10);

    let active: Vec<_> = sessions.iter().filter(|s| s.active).collect();
    let past: Vec<_> = sessions.iter().filter(|s| !s.active).collect();

    if active.is_empty() && past.is_empty() {
        content = content.push(
            text("No active hands")
                .size(FONT_BODY)
                .color(pal.text_tertiary),
        );
    }

    // Active sessions
    if !active.is_empty() {
        content = content.push(
            text("Active")
                .size(FONT_LABEL)
                .color(pal.text_secondary),
        );
        for session in &active {
            let id = Uuid::parse_str(&session.id).unwrap_or_else(|_| Uuid::nil());
            let indicator = text("\u{25CF} ") // ● dot
                .size(FONT_ICON_SM)
                .color(pal.accent);

            let label = text(&session.name).size(FONT_BODY).color(pal.text_primary);

            let btn = button(
                row![indicator, label]
                    .spacing(4)
                    .align_y(iced::Alignment::Center),
            )
            .style(button::text)
            .width(Length::Fill)
            .padding([4, 8])
            .on_press(Message::SelectAgent(id));

            let active_item = container(btn)
                .width(Length::Fill)
                .style(move |_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(pal.accent_dim)),
                    border: iced::Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });

            content = content.push(active_item);
        }
    }

    // Past sessions
    if !past.is_empty() {
        content = content.push(Space::new().height(4));
        content = content.push(
            text("History")
                .size(FONT_LABEL)
                .color(pal.text_secondary),
        );

        for session in &past {
            let can_resume = session.can_resume();

            let indicator = text("\u{25CB} ") // ○ hollow dot
                .size(FONT_ICON_SM)
                .color(pal.text_tertiary);

            let label = text(&session.name).size(FONT_BODY).color(pal.text_secondary);
            let time_label = text(format_relative_time(&session.started_at))
                .size(FONT_SMALL)
                .color(pal.text_tertiary);

            let mut session_row = row![
                indicator,
                label,
                time_label,
                Space::new().width(Length::Fill)
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center);

            if can_resume {
                let resume_btn = button(
                    text("\u{25B6}") // ▶
                        .size(FONT_ICON_SM)
                        .color(pal.accent),
                )
                .style(button::text)
                .padding([2, 4])
                .on_press(Message::ResumeAgent(session.id.clone()));

                session_row = session_row.push(resume_btn);
            }

            let delete_btn = button(
                text("\u{00D7}") // ×
                    .size(FONT_ICON)
                    .color(pal.danger),
            )
            .style(button::text)
            .padding([2, 4])
            .on_press(Message::DeleteSession(session.id.clone()));

            session_row = session_row.push(delete_btn);

            let item = container(session_row)
                .width(Length::Fill)
                .padding([4, 8])
                .style(move |_theme: &Theme| style::hovered_item(&pal));

            content = content.push(item);
        }
    }

    scrollable(content)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
}

/// Format an RFC 3339 timestamp as a short relative time string (e.g. "2h ago", "3d ago").
fn format_relative_time(rfc3339: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return String::new();
    };
    let duration = chrono::Utc::now().signed_duration_since(then);

    if duration.num_minutes() < 1 {
        "now".to_string()
    } else if duration.num_minutes() < 60 {
        format!("{}m ago", duration.num_minutes())
    } else if duration.num_hours() < 24 {
        format!("{}h ago", duration.num_hours())
    } else {
        format!("{}d ago", duration.num_days())
    }
}
