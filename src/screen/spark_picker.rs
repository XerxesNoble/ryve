// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Spark picker modal — shown when spawning a Hand so the user can assign
//! the agent to a specific spark before the terminal launches.
//!
//! Sparks are rendered hierarchically: epics act as section headers and
//! their child sparks (task/bug/feature/chore/spike/milestone) are nested
//! beneath them. Sparks with no epic parent are grouped under "(no epic)".
//! Each row carries a type badge so the user can tell at a glance whether
//! they're claiming a bug, a feature, a spike, etc.
//!
//! Includes a coding-agent chip row above the spark list so the user picks
//! the agent and the spark in a single step. Spawning is gated on both
//! being selected.

use std::collections::{HashMap, HashSet};

use data::sparks::types::Spark;
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Element, Length, Theme};

use crate::coding_agents::{CodingAgent, CompatStatus};
use crate::style::{self, FONT_BODY, FONT_HEADER, FONT_ICON_SM, FONT_LABEL, FONT_SMALL, Palette};

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

    let subtitle =
        text("Pick a coding agent and a spark. Both are required before the Hand can launch.")
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
            // Spark ryve-133ebb9b: don't even let the user pick an agent
            // whose CLI is too old — the spark would silently fail to
            // launch otherwise.
            let unsupported = agent.compatibility.is_unsupported();
            let is_selected = selected_agent == Some(agent.command.as_str());
            let chip_text_color = if unsupported {
                pal.text_tertiary
            } else if is_selected {
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
            .padding([4, 10]);
            let chip = if unsupported {
                chip
            } else {
                chip.on_press(Message::SelectAgent(agent.command.clone()))
            };
            agent_row = agent_row.push(chip);
        }
    }

    // Inline upgrade hint for any unsupported agents currently on PATH.
    // Drawn under the chip row so the message is unmissable but doesn't
    // collide with the picker layout.
    let mut upgrade_notes = column![].spacing(2);
    let mut have_unsupported = false;
    for agent in available_agents {
        if let CompatStatus::Unsupported { reason, .. } = &agent.compatibility {
            have_unsupported = true;
            upgrade_notes = upgrade_notes.push(
                text(format!("⚠ {}: {}", agent.display_name, reason))
                    .size(FONT_SMALL)
                    .color(pal.text_tertiary),
            );
        }
    }

    // Filter to actionable sparks (open, in_progress, blocked).
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
        // Group children by parent_id for fast lookup.
        let mut children_by_parent: HashMap<&str, Vec<&Spark>> = HashMap::new();
        for s in &actionable {
            if let Some(pid) = s.parent_id.as_deref() {
                children_by_parent.entry(pid).or_default().push(*s);
            }
        }

        // Collect epics (in actionable list) for the hierarchy headers.
        let epics: Vec<&Spark> = actionable
            .iter()
            .copied()
            .filter(|s| s.spark_type == "epic")
            .collect();

        // Sparks that should appear in the orphan group: non-epics whose
        // parent is either missing or not an actionable epic.
        let actionable_epic_ids: HashSet<&str> = epics.iter().map(|s| s.id.as_str()).collect();
        let orphans: Vec<&Spark> = actionable
            .iter()
            .copied()
            .filter(|s| s.spark_type != "epic")
            .filter(|s| match s.parent_id.as_deref() {
                None => true,
                Some(pid) => !actionable_epic_ids.contains(pid),
            })
            .collect();

        // Render each epic group.
        for epic in &epics {
            list = list.push(view_epic_header(epic, agent_chosen, &pal));
            if let Some(children) = children_by_parent.get(epic.id.as_str()) {
                for child in children {
                    list = list.push(view_spark_row(child, agent_chosen, &pal, true));
                }
            } else {
                list = list.push(
                    container(
                        text("(no child sparks)")
                            .size(FONT_SMALL)
                            .color(pal.text_tertiary),
                    )
                    .padding([2, 24]),
                );
            }
        }

        // Render orphans (non-epic sparks without an actionable epic parent).
        if !orphans.is_empty() {
            list = list.push(view_group_header("(no epic)", &pal));
            for s in orphans {
                list = list.push(view_spark_row(s, agent_chosen, &pal, true));
            }
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

    let mut content = column![header, subtitle, agents_label, agent_row]
        .spacing(10)
        .padding(20)
        .width(480)
        .height(520);
    if have_unsupported {
        content = content.push(upgrade_notes);
    }
    let content = content.push(scrollable_list).push(actions);

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

/// Section header for an epic. Clickable when an agent is selected so the
/// user can assign the Hand to the epic itself.
fn view_epic_header<'a>(
    epic: &'a Spark,
    agent_chosen: bool,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;
    let id = epic.id.clone();

    let title_color = if agent_chosen {
        pal.text_primary
    } else {
        pal.text_tertiary
    };

    let row_content = row![
        text("\u{25BE}") // ▾
            .size(FONT_ICON_SM)
            .color(pal.text_secondary),
        type_badge("epic", &pal),
        text(format!("P{}", epic.priority))
            .size(FONT_LABEL)
            .color(pal.text_tertiary),
        text(&epic.title).size(FONT_BODY).color(title_color),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let btn = button(row_content)
        .style(button::text)
        .width(Length::Fill)
        .padding([6, 4]);
    if agent_chosen {
        btn.on_press(Message::SelectSpark(id)).into()
    } else {
        btn.into()
    }
}

/// Non-clickable group header (used for "(no epic)" and similar).
fn view_group_header<'a>(label: &'a str, pal: &Palette) -> Element<'a, Message> {
    container(text(label).size(FONT_LABEL).color(pal.text_tertiary))
        .padding([6, 4])
        .into()
}

fn view_spark_row<'a>(
    spark: &'a Spark,
    agent_chosen: bool,
    pal: &Palette,
    indented: bool,
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
        type_badge(&spark.spark_type, &pal),
        text(priority_label)
            .size(FONT_LABEL)
            .color(pal.text_tertiary),
        text(&spark.title).size(FONT_BODY).color(title_color),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let left_pad = if indented { 24 } else { 8 };
    let btn = button(row_content)
        .style(button::text)
        .width(Length::Fill)
        .padding([4, left_pad]);
    if agent_chosen {
        btn.on_press(Message::SelectSpark(id)).into()
    } else {
        btn.into()
    }
}

// `type_badge` lives in `crate::widget::badge` so the spark picker, the
// workgraph panel, and the Activity panel all share one implementation.
// We re-export it through a thin alias here to keep call sites in this
// file readable; the call site below uses the alias.
use crate::widget::badge::type_badge;
