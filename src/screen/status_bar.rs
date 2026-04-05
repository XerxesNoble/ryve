// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Status bar — bottom bar showing branch, workshop info, and settings access.

use iced::widget::{button, container, row, text, Space};
use iced::{Element, Length, Theme};

use crate::style::{self, Palette};

#[derive(Debug, Clone)]
pub enum Message {
    OpenSettings,
}

/// Render the status bar for a workshop.
pub fn view<'a>(
    branch: Option<&'a str>,
    directory: &'a std::path::Path,
    sparks_count: usize,
    pal: &Palette,
    has_bg: bool,
) -> Element<'a, Message> {
    let pal = *pal;
    let mut items = row![].spacing(12).align_y(iced::Alignment::Center);

    // Git branch
    if let Some(branch) = branch {
        items = items.push(
            row![
                text("\u{E0A0}").size(11).color(pal.text_secondary),
                text(branch).size(11).color(pal.text_primary),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        );

        items = items.push(separator(&pal));
    }

    // Working directory (just the last component)
    let dir_name = directory
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workshop");
    items = items.push(text(dir_name).size(11).color(pal.text_secondary));

    // Workgraph spark count
    if sparks_count > 0 {
        items = items.push(separator(&pal));
        items = items.push(
            text(format!(
                "{} spark{}",
                sparks_count,
                if sparks_count == 1 { "" } else { "s" }
            ))
            .size(11)
            .color(pal.text_secondary),
        );
    }

    // Push remaining items to the right
    items = items.push(Space::new().width(Length::Fill));

    // Settings gear button (right-aligned)
    items = items.push(
        button(text("\u{2699}").size(13).color(pal.text_secondary))
            .style(button::text)
            .padding([0, 6])
            .on_press(Message::OpenSettings),
    );

    container(items.padding([3, 10]))
        .width(Length::Fill)
        .style(move |_theme: &Theme| style::status_bar_style(&pal, has_bg))
        .into()
}

fn separator<'a>(pal: &Palette) -> Element<'a, Message> {
    text("\u{2502}")
        .size(11)
        .color(pal.separator)
        .into()
}
