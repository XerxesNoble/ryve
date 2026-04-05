// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Bench panel — tabbed workspace for terminal sessions and coding agents.

use iced::widget::{Space, button, column, container, row, text};
use iced::{Element, Length, Theme};

use std::path::PathBuf;

use crate::coding_agents::CodingAgent;
use crate::style::{self, Palette};

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
    NewCodingAgent(CodingAgent),
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

            let tab_content = row![
                button(text(&tab.title).size(12).color(text_color))
                    .style(button::text)
                    .padding(0)
                    .on_press(Message::SelectTab(tab.id)),
                button(text("\u{00D7}").size(13).color(pal.text_tertiary))
                    .style(button::text)
                    .padding(0)
                    .on_press(Message::CloseTab(tab.id)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);

            let pill = container(tab_content)
                .padding([4, 10])
                .style(move |_theme: &Theme| style::tab_pill(&pal, is_active));

            tab_row = tab_row.push(pill);
        }

        let new_btn = button(text("+  \u{25BE}").size(13).color(pal.text_secondary))
            .style(button::text)
            .padding([4, 10])
            .on_press(Message::ToggleDropdown);

        tab_row = tab_row
            .push(Space::new().width(Length::Fill))
            .push(new_btn);

        tab_row.padding([4, 8]).into()
    }

    /// Render the dropdown menu (meant to be overlaid, not in flow).
    pub fn view_dropdown<'a>(
        &'a self,
        available_agents: &'a [CodingAgent],
        pal: &Palette,
    ) -> Option<Element<'a, Message>> {
        if !self.dropdown_open {
            return None;
        }

        let pal = *pal;
        let mut menu = column![].spacing(2).padding(6);

        menu = menu.push(
            button(text("New Terminal...").size(13).color(pal.text_primary))
                .style(button::text)
                .width(Length::Fill)
                .on_press(Message::NewTerminal),
        );

        for agent in available_agents {
            let label = format!("New {}...", agent.display_name);
            menu = menu.push(
                button(text(label).size(13).color(pal.text_primary))
                    .style(button::text)
                    .width(Length::Fill)
                    .on_press(Message::NewCodingAgent(agent.clone())),
            );
        }

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
