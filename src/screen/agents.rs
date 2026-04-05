// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Agents panel — lists active and past coding agent sessions.

use iced::widget::{button, column, container, row, scrollable, text, Space};
use iced::{Color, Element, Length, Theme};
use uuid::Uuid;

use crate::coding_agents::{CodingAgent, ResumeStrategy};

#[derive(Debug, Clone)]
pub enum Message {
    /// Switch to an active agent's tab.
    SelectAgent(Uuid),
    /// Resume a past (ended) agent session.
    ResumeAgent(String),
    /// Delete a past session from history.
    DeleteSession(String),
}

/// An agent session shown in the agents panel.
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

/// Render the agents panel.
pub fn view<'a>(sessions: &'a [AgentSession], has_bg: bool) -> Element<'a, Message> {
    let header = text("Agents").size(14);

    let mut content = column![header].spacing(6).padding(10);

    let active: Vec<_> = sessions.iter().filter(|s| s.active).collect();
    let past: Vec<_> = sessions.iter().filter(|s| !s.active).collect();

    if active.is_empty() && past.is_empty() {
        content = content.push(
            text("No agent sessions")
                .size(12)
                .color(Color::from_rgb(0.5, 0.5, 0.55)),
        );
    }

    // Active sessions
    if !active.is_empty() {
        content = content.push(
            text("Active")
                .size(10)
                .color(Color::from_rgb(0.5, 0.5, 0.55)),
        );
        for session in &active {
            let id = Uuid::parse_str(&session.id).unwrap_or_else(|_| Uuid::nil());
            let indicator = text("\u{25CF} ") // ● dot
                .size(10)
                .color(Color::from_rgb(0.3, 0.85, 0.4));

            let label = text(&session.name).size(12);

            let btn = button(row![indicator, label].spacing(4).align_y(iced::Alignment::Center))
                .style(button::text)
                .width(Length::Fill)
                .padding([3, 6])
                .on_press(Message::SelectAgent(id));

            content = content.push(btn);
        }
    }

    // Past sessions
    if !past.is_empty() {
        content = content.push(Space::new().height(4));
        content = content.push(
            text("History")
                .size(10)
                .color(Color::from_rgb(0.5, 0.5, 0.55)),
        );

        for session in &past {
            let can_resume = session.can_resume();

            let indicator_color = Color::from_rgb(0.45, 0.45, 0.5);
            let indicator = text("\u{25CB} ") // ○ hollow dot
                .size(10)
                .color(indicator_color);

            let name_color = Color::from_rgb(0.6, 0.6, 0.65);
            let label = text(&session.name).size(12).color(name_color);

            let session_row = if can_resume {
                let resume_btn = button(
                    text("\u{25B6}") // ▶
                        .size(10)
                        .color(Color::from_rgb(0.5, 0.75, 0.95)),
                )
                .style(button::text)
                .padding([2, 4])
                .on_press(Message::ResumeAgent(session.id.clone()));

                row![indicator, label, Space::new().width(Length::Fill), resume_btn]
                    .spacing(4)
                    .align_y(iced::Alignment::Center)
            } else {
                row![indicator, label]
                    .spacing(4)
                    .align_y(iced::Alignment::Center)
            };

            let session_style: fn(&Theme) -> container::Style = if has_bg {
                |_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(Color::from_rgba(
                        1.0, 1.0, 1.0, 0.03,
                    ))),
                    border: iced::Border {
                        radius: 3.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            } else {
                |_theme: &Theme| container::Style::default()
            };

            let item = container(session_row)
                .width(Length::Fill)
                .padding([3, 6])
                .style(session_style);

            content = content.push(item);
        }
    }

    scrollable(content)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
}
