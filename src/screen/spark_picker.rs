// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Spark picker modal — shown when spawning a Hand so the user can assign
//! the agent to a specific spark before the terminal launches.
//!
//! Includes a coding-agent chip row above the spark list so the user picks
//! the agent and the spark in a single step. Spawning is gated on both
//! being selected.

use data::sparks::types::Spark;
use iced::widget::{button, column, container, row, scrollable, text, Space};
use iced::{Element, Length, Theme};

use crate::coding_agents::CodingAgent;
use crate::style::{self, Palette, FONT_BODY, FONT_HEADER, FONT_ICON_SM, FONT_LABEL, FONT_SMALL};

// ── Messages ────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    /// User picked which coding agent to use for this Hand. Updates the
    /// picker's selection but does not spawn yet.
    SelectAgent(String),
    /// User selected a spark to assign to the new Hand. Only emitted when
    /// the picker has both a selected agent and a chosen spark.
    SelectSpark(String),
    /// Cancel — close the picker without spawning.
    Cancel,
}

// ── View ────────────────────────────────────────────────

pub fn view<'a>(
    sparks: &'a [Spark],
    available_agents: &'a [CodingAgent],
    selected_agent: Option<&'a str>,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    let title = text("Spawn Hand").size(FONT_HEADER).color(pal.text_primary);
    let close_btn = button(text("\u{00D7}").size(FONT_HEADER).color(pal.text_secondary))
        .style(button::text)
        .on_press(Message::Cancel);

    let header =
        row![title, Space::new().width(Length::Fill), close_btn].align_y(iced::Alignment::Center);

    let subtitle = text(
        "Pick a coding agent and a spark. Both are required before the Hand can launch.",
    )
    .size(FONT_SMALL)
    .color(pal.text_secondary);

    // Coding-agent chip row.
    let agents_label = text("Coding agent")
        .size(FONT_LABEL)
        .color(pal.text_tertiary);
    let mut agent_row = row![].spacing(6).align_y(iced::Alignment::Center);
    if available_agents.is_empty() {
        agent_row = agent_row.push(
            text("(no coding agents detected on PATH)")
                .size(FONT_SMALL)
                .color(pal.text_tertiary),
        );
    } else {
        for agent in available_agents {
            let is_selected = selected_agent == Some(agent.command.as_str());
            let chip_text_color = if is_selected {
                pal.window_bg
            } else {
                pal.text_primary
            };
            let chip = button(
                text(&agent.display_name)
                    .size(FONT_LABEL)
                    .color(chip_text_color),
            )
            .style(move |_t: &Theme, _s| button::Style {
                background: Some(iced::Background::Color(if is_selected {
                    pal.accent
                } else {
                    pal.surface
                })),
                text_color: chip_text_color,
                border: iced::Border {
                    color: pal.border,
                    width: 1.0,
                    radius: iced::border::Radius::from(8.0),
                },
                ..button::Style::default()
            })
            .padding([4, 10])
            .on_press(Message::SelectAgent(agent.command.clone()));
            agent_row = agent_row.push(chip);
        }
    }

    // Filter to actionable sparks (open, in_progress, blocked)
    let actionable: Vec<&Spark> = sparks
        .iter()
        .filter(|s| matches!(s.status.as_str(), "open" | "in_progress" | "blocked"))
        .collect();

    let mut list = column![].spacing(2);

    let agent_chosen = selected_agent.is_some();
    if actionable.is_empty() {
        list = list.push(
            text("No open sparks")
                .size(FONT_BODY)
                .color(pal.text_tertiary),
        );
    } else {
        for spark in actionable {
            list = list.push(view_spark_row(spark, agent_chosen, &pal));
        }
    }

    let scrollable_list = scrollable(list).height(Length::Fill);

    // Action buttons at the bottom — only Cancel; spark selection is required
    let actions = row![
        Space::new().width(Length::Fill),
        button(text("Cancel").size(FONT_LABEL).color(pal.text_tertiary))
            .style(button::text)
            .padding([6, 12])
            .on_press(Message::Cancel),
    ]
    .align_y(iced::Alignment::Center);

    let content = column![
        header,
        subtitle,
        agents_label,
        agent_row,
        scrollable_list,
        actions
    ]
    .spacing(10)
    .padding(20)
    .width(440)
    .height(460);

    let inner = container(content).style(move |_theme: &Theme| style::modal(&pal));

    // Center the modal with backdrop overlay
    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(move |_theme: &Theme| style::modal_backdrop(&pal))
        .into()
}

fn view_spark_row<'a>(
    spark: &'a Spark,
    agent_chosen: bool,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;
    let status_indicator: &str = match spark.status.as_str() {
        "open" => "\u{25CB}",        // ○
        "in_progress" => "\u{25D4}", // ◔
        "blocked" => "\u{25A0}",     // ■
        _ => "\u{25CB}",
    };

    let priority_label = format!("P{}", spark.priority);
    let id = spark.id.clone();

    let title_color = if agent_chosen {
        pal.text_primary
    } else {
        pal.text_tertiary
    };

    let row_content = row![
        text(status_indicator)
            .size(FONT_ICON_SM)
            .color(pal.text_secondary),
        text(priority_label)
            .size(FONT_LABEL)
            .color(pal.text_tertiary),
        text(&spark.title).size(FONT_BODY).color(title_color),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let btn = button(row_content)
        .style(button::text)
        .width(Length::Fill)
        .padding([6, 8]);
    if agent_chosen {
        btn.on_press(Message::SelectSpark(id)).into()
    } else {
        btn.into()
    }
}
