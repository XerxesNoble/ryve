// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Background picker — modal overlay for choosing a workshop background image.
//! Supports local file upload and Unsplash search.

use std::collections::HashMap;

use data::unsplash::Photo;
use iced::widget::{
    button, column, container, image, row, rule, scrollable, text, text_input, Space,
};
use iced::{Element, Length, Theme};

use crate::style::{self, Palette};

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

pub fn view<'a>(state: &'a PickerState, pal: &Palette, has_background: bool) -> Element<'a, Message> {
    let pal = *pal;

    let title = text("Workshop Settings").size(18).color(pal.text_primary);
    let close_btn = button(text("\u{00D7}").size(18).color(pal.text_secondary))
        .style(button::text)
        .on_press(Message::Close);

    let header = row![title, Space::new().width(Length::Fill), close_btn]
        .align_y(iced::Alignment::Center);

    let mut content = column![header, rule::horizontal(1)].spacing(12);

    // Upload section
    content = content.push(
        button(text("Upload from file...").size(13))
            .style(button::secondary)
            .padding([8, 16])
            .on_press(Message::PickLocalFile),
    );

    // Remove button (if a background is set)
    if has_background {
        content = content.push(
            button(text("Remove background").size(13))
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
            .size(13)
            .padding(8);

        let search_btn = button(text("Search").size(13))
            .style(button::primary)
            .padding([8, 16])
            .on_press(Message::Search);

        let search_row = row![search_input, search_btn]
            .spacing(8)
            .align_y(iced::Alignment::Center);

        content = content.push(search_row);

        if state.loading {
            content = content.push(text("Searching...").size(12).color(pal.text_secondary));
        } else if state.results.is_empty() && !state.query.is_empty() {
            content = content.push(text("No results").size(12).color(pal.text_secondary));
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
                    .size(10)
                    .color(pal.text_tertiary),
            );
        }
    } else {
        content = content.push(
            text("Set UNSPLASH_ACCESS_KEY to search Unsplash")
                .size(12)
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

fn view_thumbnail<'a>(state: &'a PickerState, photo: &'a Photo, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let content: Element<'a, Message> = if let Some(handle) = state.thumbnails.get(&photo.id) {
        column![
            image(handle.clone())
                .width(Length::Fill)
                .height(100)
                .content_fit(iced::ContentFit::Cover),
            text(&photo.photographer)
                .size(9)
                .color(pal.text_tertiary),
        ]
        .spacing(2)
        .into()
    } else {
        container(text("...").size(11).color(pal.text_tertiary))
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
