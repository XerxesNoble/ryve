// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Head picker modal — shown when the user clicks "+ → New Head" in the
//! bench dropdown. Lets the user choose which coding agent to launch as
//! the Head, plus an optional one-line goal that gets injected into the
//! Head's system prompt.
//!
//! No spark assignment is involved — the Head's job is to *create* sparks,
//! not to claim an existing one.

use iced::widget::{button, column, container, row, text, text_input, Space};
use iced::{Element, Length, Theme};

use crate::coding_agents::CodingAgent;
use crate::style::{self, Palette, FONT_BODY, FONT_HEADER, FONT_LABEL, FONT_SMALL};

#[derive(Debug, Clone)]
pub enum Message {
    /// User typed in the goal textbox.
    GoalChanged(String),
    /// User picked a coding agent — spawn the Head with this agent and the
    /// goal currently in the textbox.
    SelectAgent(String),
    /// Cancel — close the picker.
    Cancel,
}

/// Picker state. Lives on `Workshop` so it survives across `view` calls.
#[derive(Debug, Default, Clone)]
pub struct PickerState {
    pub goal: String,
}

pub fn view<'a>(
    state: &'a PickerState,
    available_agents: &'a [CodingAgent],
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    let title = text("Spawn Head").size(FONT_HEADER).color(pal.text_primary);
    let close_btn = button(text("\u{00D7}").size(FONT_HEADER).color(pal.text_secondary))
        .style(button::text)
        .on_press(Message::Cancel);
    let header =
        row![title, Space::new().width(Length::Fill), close_btn].align_y(iced::Alignment::Center);

    let subtitle = text(
        "A Head is a coding agent that decomposes a goal into sparks, spawns Hands, and \
         finally spawns a Merger to integrate the result into a single PR.",
    )
    .size(FONT_SMALL)
    .color(pal.text_secondary);

    let goal_label = text("What should the crew build?")
        .size(FONT_LABEL)
        .color(pal.text_tertiary);
    let goal_input = text_input("e.g. add user profile editing", &state.goal)
        .on_input(Message::GoalChanged)
        .padding(8)
        .size(FONT_BODY);

    let agents_label = text("Coding agent for the Head")
        .size(FONT_LABEL)
        .color(pal.text_tertiary);

    let mut agent_list = column![].spacing(4);
    if available_agents.is_empty() {
        agent_list = agent_list.push(
            text("(no coding agents detected on PATH — install claude/codex/aider/opencode)")
                .size(FONT_SMALL)
                .color(pal.text_tertiary),
        );
    } else {
        for agent in available_agents {
            let row_content = row![
                text(&agent.display_name)
                    .size(FONT_BODY)
                    .color(pal.text_primary),
                Space::new().width(Length::Fill),
                text(&agent.command)
                    .size(FONT_SMALL)
                    .color(pal.text_tertiary),
            ]
            .align_y(iced::Alignment::Center);
            agent_list = agent_list.push(
                button(row_content)
                    .style(button::text)
                    .width(Length::Fill)
                    .padding([6, 8])
                    .on_press(Message::SelectAgent(agent.command.clone())),
            );
        }
    }

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
        goal_label,
        goal_input,
        agents_label,
        agent_list,
        actions
    ]
    .spacing(10)
    .padding(20)
    .width(440);

    let inner = container(content).style(move |_t: &Theme| style::modal(&pal));

    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(move |_t: &Theme| style::modal_backdrop(&pal))
        .into()
}
