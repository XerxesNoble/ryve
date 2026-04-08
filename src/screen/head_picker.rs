// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Head picker modal — shown when the user clicks "+ → New Head" in the
//! bench dropdown. Lets the user choose which coding agent to launch as
//! the Head, plus an optional epic for the Head to work under.
//!
//! No spark *claim* is involved — the Head's job is to *create* child
//! sparks under an epic and spawn Hands for them, not to claim an
//! existing spark itself. Picking an epic is optional: if the user skips
//! it, the Head will create its own parent epic from scratch.

use data::sparks::types::Spark;
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Element, Length, Theme};

use crate::coding_agents::{CodingAgent, CompatStatus};
use crate::style::{self, FONT_BODY, FONT_HEADER, FONT_LABEL, FONT_SMALL, Palette};

/// Subtitle of the Head picker modal. Frames Heads as a tool Atlas (the
/// Director) usually delegates to on the user's behalf, so the picker
/// reinforces the Atlas → Heads → Hands hierarchy.
pub const HEAD_PICKER_SUBTITLE: &str =
    "Atlas, your Director, normally delegates work for you. Spawn a Head directly when you \
     want a coding agent to decompose an epic into sparks, spawn Hands, and finally spawn a \
     Merger to integrate the result into a single PR.";

#[derive(Debug, Clone)]
pub enum Message {
    /// User picked (or cleared) the optional parent epic. `None` means no
    /// epic — the Head will mint its own.
    SelectEpic(Option<String>),
    /// User picked a coding agent — spawn the Head with this agent and the
    /// currently-selected epic (if any).
    SelectAgent(String),
    /// Cancel — close the picker.
    Cancel,
}

/// Picker state. Lives on `Workshop` so it survives across `view` calls.
#[derive(Debug, Default, Clone)]
pub struct PickerState {
    /// Optionally selected parent epic id. When set, the Head is told to
    /// decompose this epic into child sparks rather than mint its own.
    pub selected_epic_id: Option<String>,
}

pub fn view<'a>(
    state: &'a PickerState,
    sparks: &'a [Spark],
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

    let subtitle = text(HEAD_PICKER_SUBTITLE)
        .size(FONT_SMALL)
        .color(pal.text_secondary);

    // ── epic list ──
    // Spark ryve-0742ef5a: instead of a free-form goal textbox, show the
    // actual epics in the workshop so the user can pick one for the Head
    // to work on. Selection is optional — "(no epic)" lets the Head mint
    // its own parent epic from the conversation it has with the user.
    let epic_label = text("Parent epic (optional)")
        .size(FONT_LABEL)
        .color(pal.text_tertiary);

    let epics: Vec<&Spark> = sparks
        .iter()
        .filter(|s| s.spark_type == "epic" && s.status != "closed")
        .collect();

    let mut epic_list = column![].spacing(2);

    // "(no epic)" row — always present, always selectable.
    {
        let selected = state.selected_epic_id.is_none();
        epic_list = epic_list.push(epic_row(
            "(no epic — Head will create its own)",
            None,
            selected,
            &pal,
            Message::SelectEpic(None),
        ));
    }

    if epics.is_empty() {
        epic_list = epic_list.push(
            text("No open epics yet.")
                .size(FONT_SMALL)
                .color(pal.text_tertiary),
        );
    } else {
        for epic in &epics {
            let id = epic.id.clone();
            let selected = state.selected_epic_id.as_deref() == Some(epic.id.as_str());
            epic_list = epic_list.push(epic_row(
                &epic.title,
                Some(&format!("P{}", epic.priority)),
                selected,
                &pal,
                Message::SelectEpic(Some(id)),
            ));
        }
    }

    let epic_scroll = scrollable(epic_list).height(Length::Fixed(180.0));

    // ── agent list ──
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
            // Spark ryve-133ebb9b: gate selection on the version probe.
            // Unsupported agents show their detected version + a "please
            // upgrade" hint and the row is non-clickable so the user can't
            // launch a Head that will crash on first contact with the CLI.
            let unsupported = agent.compatibility.is_unsupported();
            let name_color = if unsupported {
                pal.text_tertiary
            } else {
                pal.text_primary
            };
            let row_content = row![
                text(&agent.display_name).size(FONT_BODY).color(name_color),
                Space::new().width(Length::Fill),
                text(&agent.command)
                    .size(FONT_SMALL)
                    .color(pal.text_tertiary),
            ]
            .align_y(iced::Alignment::Center);
            let mut row_col = column![row_content].spacing(2);
            if let CompatStatus::Unsupported { version, .. } = &agent.compatibility {
                row_col = row_col.push(
                    text(format!("v{version} — upgrade required"))
                        .size(FONT_SMALL)
                        .color(pal.text_tertiary),
                );
            }
            let btn = button(row_col)
                .style(button::text)
                .width(Length::Fill)
                .padding([6, 8]);
            agent_list = agent_list.push(if unsupported {
                btn
            } else {
                btn.on_press(Message::SelectAgent(agent.command.clone()))
            });
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
        epic_label,
        epic_scroll,
        agents_label,
        agent_list,
        actions
    ]
    .spacing(10)
    .padding(20)
    .width(480);

    let inner = container(content).style(move |_t: &Theme| style::modal(&pal));

    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(move |_t: &Theme| style::modal_backdrop(&pal))
        .into()
}

/// One selectable row in the epic list. Highlighted when `selected` is true.
fn epic_row<'a>(
    title: &str,
    priority_label: Option<&str>,
    selected: bool,
    pal: &Palette,
    on_press: Message,
) -> Element<'a, Message> {
    let pal = *pal;
    let title_color = if selected {
        pal.window_bg
    } else {
        pal.text_primary
    };
    let prio_color = if selected {
        pal.window_bg
    } else {
        pal.text_tertiary
    };

    let mut row_content = row![].spacing(8).align_y(iced::Alignment::Center);
    if let Some(p) = priority_label {
        row_content = row_content.push(text(p.to_string()).size(FONT_LABEL).color(prio_color));
    }
    row_content = row_content.push(text(title.to_string()).size(FONT_BODY).color(title_color));

    button(row_content)
        .style(move |_t: &Theme, _s| button::Style {
            background: Some(iced::Background::Color(if selected {
                pal.accent
            } else {
                pal.surface
            })),
            text_color: title_color,
            border: iced::Border {
                color: pal.border,
                width: 1.0,
                radius: iced::border::Radius::from(6.0),
            },
            ..button::Style::default()
        })
        .width(Length::Fill)
        .padding([5, 10])
        .on_press(on_press)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spark ryve-7aa4dcd8: the Head picker subtitle frames Heads in terms of
    /// Atlas (Director). Lock the wording so a future copy edit can't quietly
    /// drop Atlas from this surface.
    #[test]
    fn head_picker_subtitle_names_atlas_as_director() {
        assert!(HEAD_PICKER_SUBTITLE.contains("Atlas"));
        assert!(HEAD_PICKER_SUBTITLE.contains("Director"));
        assert!(HEAD_PICKER_SUBTITLE.contains("delegates"));
    }
}
