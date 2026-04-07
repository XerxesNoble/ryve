// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Bench panel — tabbed workspace for terminal sessions and coding agents.

use iced::widget::{
    Space, button, column, container, row, scrollable, text, tooltip,
};
use iced::{Element, Length, Theme};

use std::path::PathBuf;

use crate::coding_agents::CodingAgent;
use crate::style::{self, Palette, FONT_BODY, FONT_HEADER, FONT_ICON, FONT_ICON_SM, FONT_LABEL, FONT_SMALL};
use data::ryve_dir::AgentDef;

/// A tab in the bench — either a plain terminal, coding agent, or file viewer.
#[derive(Debug, Clone)]
pub struct Tab {
    pub id: u64,
    pub title: String,
    pub kind: TabKind,
}

#[derive(Debug, Clone)]
pub enum TabKind {
    Terminal,
    CodingAgent(CodingAgent),
    FileViewer(PathBuf),
}

/// State for the bench panel.
pub struct BenchState {
    pub tabs: Vec<Tab>,
    pub active_tab: Option<u64>,
    pub dropdown_open: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    SelectTab(u64),
    CloseTab(u64),
    ToggleDropdown,
    NewTerminal,
    /// Open the spark+agent picker for spawning a regular Hand on a spark.
    NewHand,
    /// Open the agent picker for spawning a Head — a coding agent that
    /// orchestrates a Crew of Hands. No spark is selected; the Head creates
    /// its own.
    NewHead,
    /// Legacy: directly spawn a coding agent of the given type. Still emitted
    /// from the Hand picker once the user has chosen both the spark and the
    /// agent.
    NewCodingAgent(CodingAgent),
    NewCustomAgent(usize),
    TerminalEvent(iced_term::Event),
}

impl BenchState {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active_tab: None,
            dropdown_open: false,
        }
    }

    /// Create a tab with an externally-assigned ID.
    pub fn create_tab(&mut self, id: u64, title: String, kind: TabKind) {
        self.tabs.push(Tab { id, title, kind });
        self.active_tab = Some(id);
        self.dropdown_open = false;
    }

    pub fn close_tab(&mut self, id: u64) {
        self.tabs.retain(|t| t.id != id);
        if self.active_tab == Some(id) {
            self.active_tab = self.tabs.last().map(|t| t.id);
        }
    }

    /// Render the tab bar row with liquid glass pill tabs.
    pub fn view_tab_bar(&self, pal: &Palette) -> Element<'_, Message> {
        let pal = *pal;
        let mut tab_row = row![].spacing(4).align_y(iced::Alignment::Center);

        for tab in &self.tabs {
            let is_active = self.active_tab == Some(tab.id);
            let text_color = if is_active {
                pal.text_primary
            } else {
                pal.text_secondary
            };

            let (kind_icon, tip_text) = match &tab.kind {
                TabKind::Terminal => ("\u{25B8}", "Terminal".to_string()),
                TabKind::CodingAgent(agent) => ("\u{2726}", agent.display_name.clone()),
                TabKind::FileViewer(path) => ("\u{25A2}", path.to_string_lossy().to_string()),
            };

            let tab_content = row![
                text(kind_icon).size(FONT_ICON_SM).color(text_color),
                button(text(&tab.title).size(FONT_BODY).color(text_color))
                    .style(button::text)
                    .padding(0)
                    .on_press(Message::SelectTab(tab.id)),
                button(text("\u{00D7}").size(FONT_ICON).color(pal.text_tertiary))
                    .style(button::text)
                    .padding(0)
                    .on_press(Message::CloseTab(tab.id)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);

            let pill = container(tab_content)
                .padding([4, 10])
                .style(move |_theme: &Theme| style::tab_pill(&pal, is_active));

            tab_row = tab_row.push(
                tooltip(pill, text(tip_text).size(FONT_SMALL), tooltip::Position::Bottom)
                    .gap(4)
                    .style(move |_theme: &Theme| style::dropdown(&pal)),
            );
        }

        let new_btn = button(text("+  \u{25BE}").size(FONT_ICON).color(pal.text_secondary))
            .style(button::text)
            .padding([4, 10])
            .on_press(Message::ToggleDropdown);

        // Wrap the tabs in a horizontal scrollable so long tab lists don't push
        // the "+" button offscreen. The scrollable fills the available width;
        // the "+" button stays pinned on the right, always reachable.
        let scrollable_tabs = scrollable(tab_row)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(4).scroller_width(4),
            ))
            .width(Length::Fill);

        row![scrollable_tabs, new_btn]
            .align_y(iced::Alignment::Center)
            .spacing(4)
            .padding([4, 8])
            .into()
    }

    /// Render the dropdown menu (meant to be overlaid, not in flow).
    pub fn view_dropdown<'a>(
        &'a self,
        available_agents: &'a [CodingAgent],
        custom_agents: &'a [AgentDef],
        pal: &Palette,
    ) -> Option<Element<'a, Message>> {
        if !self.dropdown_open {
            return None;
        }

        let pal = *pal;
        let mut menu = column![].spacing(2).padding(6);

        // Top-level: the three roles a new tab can take on. The agent
        // picker happens inside the spark picker for Hand / inside its own
        // tiny picker for Head.
        let any_agent_available = !available_agents.is_empty();

        let head_button = button(
            text("New Head...").size(FONT_BODY).color(if any_agent_available {
                pal.text_primary
            } else {
                pal.text_tertiary
            }),
        )
        .style(button::text)
        .width(Length::Fill);
        let head_button = if any_agent_available {
            head_button.on_press(Message::NewHead)
        } else {
            head_button
        };
        menu = menu.push(head_button);

        let hand_button = button(
            text("New Hand...").size(FONT_BODY).color(if any_agent_available {
                pal.text_primary
            } else {
                pal.text_tertiary
            }),
        )
        .style(button::text)
        .width(Length::Fill);
        let hand_button = if any_agent_available {
            hand_button.on_press(Message::NewHand)
        } else {
            hand_button
        };
        menu = menu.push(hand_button);

        menu = menu.push(
            button(text("New Terminal...").size(FONT_BODY).color(pal.text_primary))
                .style(button::text)
                .width(Length::Fill)
                .on_press(Message::NewTerminal),
        );

        if !any_agent_available {
            menu = menu.push(
                text("(install claude/codex/aider/opencode to enable Head & Hand)")
                    .size(FONT_SMALL)
                    .color(pal.text_tertiary),
            );
        }

        // Custom agents still get their own quick-launch entries — they
        // bypass the picker because the user explicitly named the agent
        // when they registered it under .ryve/agents/.
        if !custom_agents.is_empty() {
            menu = menu.push(
                text("Custom").size(FONT_LABEL).color(pal.text_tertiary),
            );
            for (i, def) in custom_agents.iter().enumerate() {
                let label = format!("New {}...", def.name);
                menu = menu.push(
                    button(text(label).size(FONT_BODY).color(pal.text_primary))
                        .style(button::text)
                        .width(Length::Fill)
                        .on_press(Message::NewCustomAgent(i)),
                );
            }
        }

        // Silence "unused parameter" warnings until the legacy direct-spawn
        // dropdown items are removed in a follow-up spark.
        let _ = available_agents;

        let dropdown = container(menu)
            .style(move |_theme: &Theme| style::dropdown(&pal))
            .width(220);

        Some(
            row![Space::new().width(Length::Fill), dropdown]
                .width(Length::Fill)
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Palette;

    #[test]
    fn tab_bar_handles_many_tabs_without_panic() {
        // Regression test for sp-ux0010: with many tabs the row used to push
        // the "+" button offscreen. The fix wraps the tabs in a horizontal
        // scrollable. This test ensures view_tab_bar still constructs cleanly
        // for a tab count well beyond the previous overflow threshold (~8).
        let mut bench = BenchState::new();
        for i in 0..50 {
            bench.create_tab(i, format!("tab-{i}"), TabKind::Terminal);
        }
        assert_eq!(bench.tabs.len(), 50);
        let pal = Palette::dark();
        let _element = bench.view_tab_bar(&pal);
    }

    #[test]
    fn tab_bar_renders_with_zero_tabs() {
        let bench = BenchState::new();
        let pal = Palette::dark();
        let _element = bench.view_tab_bar(&pal);
    }
}
