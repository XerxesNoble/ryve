// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Background picker — modal overlay for choosing a workshop background image.
//! Supports local file upload and Unsplash search.

use std::collections::HashMap;

use data::unsplash::Photo;
use iced::widget::{
    Space, button, column, container, image, row, rule, scrollable, text, text_input,
};
use iced::{Element, Length, Theme};

use crate::style::{self, Palette, FONT_BODY, FONT_HEADER, FONT_LABEL, FONT_SMALL};

// ── Messages ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    /// Close the picker without changes.
    Close,

    /// User wants to upload a local file.
    PickLocalFile,

    /// Unsplash search query text changed.
    QueryChanged(String),
    /// Trigger search.
    Search,
    /// Search results arrived (thumbnails loaded separately).
    SearchResults(Vec<Photo>),
    /// A thumbnail's bytes finished loading.
    ThumbnailLoaded(String, Vec<u8>),
    /// User selected an Unsplash photo.
    SelectPhoto(Photo),

    /// Remove the current background.
    RemoveBackground,

    // ── Agent settings ───────────────────────────────────
    /// Set the default agent command (or None to clear).
    SetDefaultAgent(Option<String>),
    /// Toggle full-auto mode for a specific agent command.
    ToggleFullAuto(String),
}

// ── State ───────────────────────────────────���─────────

#[derive(Debug, Clone)]
pub struct PickerState {
    pub open: bool,
    pub query: String,
    pub results: Vec<Photo>,
    /// Thumbnail image handles keyed by photo ID.
    pub thumbnails: HashMap<String, image::Handle>,
    pub loading: bool,
    pub has_unsplash_key: bool,
}

impl PickerState {
    pub fn new() -> Self {
        let has_unsplash_key = std::env::var("UNSPLASH_ACCESS_KEY").is_ok();
        Self {
            open: false,
            query: String::new(),
            results: Vec::new(),
            thumbnails: HashMap::new(),
            loading: false,
            has_unsplash_key,
        }
    }
}

// ── View ──────────────────────────────────────────────

/// Info about an available agent, passed in for rendering.
pub struct AgentInfo {
    pub command: String,
    pub display_name: String,
    pub full_auto: bool,
    pub is_default: bool,
}

pub fn view<'a>(
    state: &'a PickerState,
    pal: &Palette,
    has_background: bool,
    agents: Vec<AgentInfo>,
) -> Element<'a, Message> {
    let pal = *pal;

    let title = text("Workshop Settings").size(FONT_HEADER).color(pal.text_primary);
    let close_btn = button(text("\u{00D7}").size(FONT_HEADER).color(pal.text_secondary))
        .style(button::text)
        .on_press(Message::Close);

    let header =
        row![title, Space::new().width(Length::Fill), close_btn].align_y(iced::Alignment::Center);

    let mut content = column![header, rule::horizontal(1)].spacing(12);

    // ── Agent Settings Section ───────────────────────────
    content = content.push(
        text("Coding Agents")
            .size(14)
            .color(pal.text_primary),
    );

    // Default agent selector
    {
        content = content.push(
            text("Default agent (⌘H)")
                .size(12)
                .color(pal.text_secondary),
        );

        let mut agent_row = row![].spacing(6);

        // "None" button
        let none_active = agents.iter().all(|a| !a.is_default);
        agent_row = agent_row.push(
            button(text("None").size(12))
                .style(if none_active { button::primary } else { button::secondary })
                .padding([4, 10])
                .on_press(Message::SetDefaultAgent(None)),
        );

        for agent in &agents {
            let is_selected = agent.is_default;
            agent_row = agent_row.push(
                button(text(agent.display_name.clone()).size(12))
                    .style(if is_selected { button::primary } else { button::secondary })
                    .padding([4, 10])
                    .on_press(Message::SetDefaultAgent(Some(agent.command.clone()))),
            );
        }

        content = content.push(agent_row);
    }

    // Per-agent full-auto toggles
    if !agents.is_empty() {
        content = content.push(
            text("Full-auto mode")
                .size(12)
                .color(pal.text_secondary),
        );

        let mut auto_row = row![].spacing(6);
        for agent in &agents {
            let label = if agent.full_auto {
                format!("✓ {}", agent.display_name)
            } else {
                agent.display_name.clone()
            };
            auto_row = auto_row.push(
                button(text(label).size(12))
                    .style(if agent.full_auto { button::success } else { button::secondary })
                    .padding([4, 10])
                    .on_press(Message::ToggleFullAuto(agent.command.clone())),
            );
        }
        content = content.push(auto_row);
    }

    content = content.push(rule::horizontal(1));

    // ── Background Section ───────────────────────────────
    content = content.push(
        text("Background")
            .size(14)
            .color(pal.text_primary),
    );

    // Upload section
    content = content.push(
        button(text("Upload from file...").size(FONT_BODY))
            .style(button::secondary)
            .padding([8, 16])
            .on_press(Message::PickLocalFile),
    );

    // Remove button (if a background is set)
    if has_background {
        content = content.push(
            button(text("Remove background").size(FONT_BODY))
                .style(button::danger)
                .padding([8, 16])
                .on_press(Message::RemoveBackground),
        );
    }

    content = content.push(rule::horizontal(1));

    // Unsplash section
    if state.has_unsplash_key {
        let search_input = text_input("Search Unsplash...", &state.query)
            .on_input(Message::QueryChanged)
            .on_submit(Message::Search)
            .size(FONT_BODY)
            .padding(8);

        let search_btn = button(text("Search").size(FONT_BODY))
            .style(button::primary)
            .padding([8, 16])
            .on_press(Message::Search);

        let search_row = row![search_input, search_btn]
            .spacing(8)
            .align_y(iced::Alignment::Center);

        content = content.push(search_row);

        if state.loading {
            content = content.push(text("Searching...").size(FONT_LABEL).color(pal.text_secondary));
        } else if state.results.is_empty() && !state.query.is_empty() {
            content = content.push(text("No results").size(FONT_LABEL).color(pal.text_secondary));
        }

        // Thumbnail grid (3 columns)
        if !state.results.is_empty() {
            let mut grid = column![].spacing(8);
            for chunk in state.results.chunks(3) {
                let mut grid_row = row![].spacing(8);
                for photo in chunk {
                    grid_row = grid_row.push(view_thumbnail(state, photo, &pal));
                }
                // Fill remaining cells if fewer than 3
                for _ in chunk.len()..3 {
                    grid_row = grid_row.push(Space::new().width(Length::FillPortion(1)));
                }
                grid = grid.push(grid_row);
            }

            content = content.push(scrollable(grid).height(Length::Fill));

            content = content.push(
                text("Photos provided by Unsplash")
                    .size(FONT_SMALL)
                    .color(pal.text_tertiary),
            );
        }
    } else {
        content = content.push(
            text("Set UNSPLASH_ACCESS_KEY to search Unsplash")
                .size(FONT_LABEL)
                .color(pal.text_tertiary),
        );
    }

    let inner = container(content.spacing(12).padding(20).width(500).height(500))
        .style(move |_theme: &Theme| style::modal(&pal));

    // Center the modal with backdrop overlay
    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(move |_theme: &Theme| style::modal_backdrop(&pal))
        .into()
}

fn view_thumbnail<'a>(
    state: &'a PickerState,
    photo: &'a Photo,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;
    let content: Element<'a, Message> = if let Some(handle) = state.thumbnails.get(&photo.id) {
        column![
            image(handle.clone())
                .width(Length::Fill)
                .height(100)
                .content_fit(iced::ContentFit::Cover),
            text(&photo.photographer)
                .size(FONT_SMALL)
                .color(pal.text_tertiary),
        ]
        .spacing(2)
        .into()
    } else {
        container(text("...").size(FONT_SMALL).color(pal.text_tertiary))
            .width(Length::Fill)
            .height(100)
            .center(Length::Fill)
            .into()
    };

    button(content)
        .style(button::secondary)
        .padding(4)
        .width(Length::FillPortion(1))
        .on_press(Message::SelectPhoto(photo.clone()))
        .into()
}
