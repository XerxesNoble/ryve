// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Confirmation dialog shown when the user closes a workshop tab while
//! one or more Hands (coding-agent sessions) are still active. Spark
//! sp-ux0021 — without this, hitting the close button silently kills
//! every running terminal/agent in the workshop.

use iced::widget::{Space, button, column, container, row, text};
use iced::{Element, Length, Theme};

use crate::style::{self, FONT_BODY, FONT_HEADER, FONT_LABEL, Palette};

#[derive(Debug, Clone)]
pub enum Message {
    /// Proceed with closing the workshop at the given index.
    Confirm(usize),
    /// Dismiss the dialog and leave the workshop open.
    Cancel,
}

/// Render the modal. `workshop_idx` and `workshop_name` describe the tab
/// being closed; `active_hands` is the count of currently-active Hands.
pub fn view<'a>(
    workshop_idx: usize,
    workshop_name: &str,
    active_hands: usize,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    let title = text("Close workshop?")
        .size(FONT_HEADER)
        .color(pal.text_primary);

    let body = text(body_text(workshop_name, active_hands))
        .size(FONT_BODY)
        .color(pal.text_secondary);

    let cancel_btn = button(text("Cancel").size(FONT_LABEL).color(pal.text_primary))
        .style(button::text)
        .padding([6, 14])
        .on_press(Message::Cancel);

    let confirm_btn = button(
        text("Close anyway")
            .size(FONT_LABEL)
            .color(pal.window_bg),
    )
    .style(move |_t: &Theme, _s| button::Style {
        background: Some(iced::Background::Color(pal.danger)),
        text_color: pal.window_bg,
        border: iced::Border {
            color: pal.danger,
            width: 1.0,
            radius: iced::border::Radius::from(6.0),
        },
        ..button::Style::default()
    })
    .padding([6, 14])
    .on_press(Message::Confirm(workshop_idx));

    let actions = row![Space::new().width(Length::Fill), cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(iced::Alignment::Center);

    let content = column![title, body, actions]
        .spacing(14)
        .padding(20)
        .width(420);

    let inner = container(content).style(move |_t: &Theme| style::modal(&pal));

    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(move |_t: &Theme| style::modal_backdrop(&pal))
        .into()
}

/// Build the body text shown in the confirmation dialog. Extracted so it
/// can be unit-tested without spinning up the iced renderer.
pub fn body_text(workshop_name: &str, active_hands: usize) -> String {
    let hand_word = if active_hands == 1 { "Hand" } else { "Hands" };
    format!(
        "{} has {} active {}. Closing will terminate their terminals and \
         agent processes.",
        workshop_name, active_hands, hand_word
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_text_singular_hand() {
        let s = body_text("Ryve", 1);
        assert!(s.contains("1 active Hand."), "got: {s}");
        assert!(!s.contains("Hands."));
    }

    #[test]
    fn body_text_plural_hands() {
        let s = body_text("Ryve", 3);
        assert!(s.contains("3 active Hands."), "got: {s}");
    }

    #[test]
    fn body_text_includes_workshop_name() {
        let s = body_text("my-project", 2);
        assert!(s.starts_with("my-project "), "got: {s}");
    }
}

