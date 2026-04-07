// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Ember notification bar.
//!
//! Renders the workshop's active **embers** — ephemeral inter-agent signals
//! produced when a Hand finishes, a required contract fails, or a spark is
//! blocked — as a horizontal strip of dismissible cards. Unlike the generic
//! `toast` overlay (used for local UI errors), this bar is backed by the
//! workgraph database: the 3-second sparks poll populates `Workshop::embers`,
//! and the dismiss button deletes the row via `ember_repo::delete`.
//!
//! Color convention (from the spark description):
//! - `Glow` → blue  (informational — Hand finished)
//! - `Flash` → yellow (attention — spark blocked)
//! - `Flare` → orange (warning — contract failed)
//! - `Blaze` → red  (critical — reserved for future use)
//! - `Ash`  → gray  (archived; not surfaced here)

use data::sparks::types::Ember;
use iced::widget::{Space, button, column, container, row, text};
use iced::{Background, Border, Color, Element, Length, Theme};

use crate::style::{FONT_LABEL, FONT_SMALL, Palette};

#[derive(Debug, Clone)]
pub enum Message {
    /// User clicked the × button on an ember card.
    Dismiss(String),
}

/// Decide the accent color for an ember based on its `ember_type` string.
pub fn accent_for(ember_type: &str) -> Color {
    match ember_type {
        // Glow — informational blue
        "glow" => Color {
            r: 0.26,
            g: 0.60,
            b: 0.98,
            a: 1.0,
        },
        // Flash — yellow
        "flash" => Color {
            r: 1.0,
            g: 0.82,
            b: 0.20,
            a: 1.0,
        },
        // Flare — orange
        "flare" => Color {
            r: 0.98,
            g: 0.55,
            b: 0.18,
            a: 1.0,
        },
        // Blaze — red
        "blaze" => Color {
            r: 0.95,
            g: 0.30,
            b: 0.28,
            a: 1.0,
        },
        // Ash or unknown — muted gray
        _ => Color {
            r: 0.55,
            g: 0.55,
            b: 0.58,
            a: 1.0,
        },
    }
}

/// Short label for the ember type, used as the badge on each card.
pub fn label_for(ember_type: &str) -> &'static str {
    match ember_type {
        "glow" => "GLOW",
        "flash" => "FLASH",
        "flare" => "FLARE",
        "blaze" => "BLAZE",
        "ash" => "ASH",
        _ => "EMBER",
    }
}

/// Render the ember notification bar. Returns `None` when the workshop has
/// no active embers so the caller can skip adding an empty row to the layout.
pub fn view<'a>(embers: &'a [Ember], pal: &Palette) -> Option<Element<'a, Message>> {
    if embers.is_empty() {
        return None;
    }
    let pal = *pal;

    let mut stack = column![].spacing(6);
    for e in embers {
        stack = stack.push(ember_card(e, &pal));
    }

    Some(
        container(stack)
            .padding([6, 10])
            .width(Length::Fill)
            .into(),
    )
}

fn ember_card<'a>(e: &'a Ember, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let accent = accent_for(&e.ember_type);

    let badge = container(
        text(label_for(&e.ember_type))
            .size(FONT_SMALL)
            .color(Color::WHITE),
    )
    .padding([2, 6])
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(accent)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 4.0.into(),
        },
        text_color: Some(Color::WHITE),
        ..Default::default()
    });

    let source = e
        .source_agent
        .clone()
        .unwrap_or_else(|| "system".to_string());

    let dismiss = button(
        text("\u{00D7}")
            .size(FONT_LABEL)
            .color(pal.text_secondary),
    )
    .style(button::text)
    .padding([0, 4])
    .on_press(Message::Dismiss(e.id.clone()));

    let header = row![
        badge,
        text(source).size(FONT_SMALL).color(pal.text_tertiary),
        text(e.content.clone())
            .size(FONT_LABEL)
            .color(pal.text_primary),
        Space::new().width(Length::Fill),
        dismiss,
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    container(header)
        .padding([6, 10])
        .width(Length::Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(Color {
                r: pal.window_bg.r,
                g: pal.window_bg.g,
                b: pal.window_bg.b,
                a: 0.88,
            })),
            border: Border {
                color: Color { a: 0.65, ..accent },
                width: 1.0,
                radius: 6.0.into(),
            },
            text_color: Some(pal.text_primary),
            ..Default::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ember(id: &str, ty: &str) -> Ember {
        Ember {
            id: id.to_string(),
            ember_type: ty.to_string(),
            content: format!("content-{id}"),
            source_agent: Some("hand-x".to_string()),
            workshop_id: "ws".to_string(),
            ttl_seconds: 3600,
            created_at: "2026-04-07T00:00:00+00:00".to_string(),
        }
    }

    #[test]
    fn view_returns_none_when_empty() {
        let pal = Palette::dark();
        assert!(view(&[], &pal).is_none());
    }

    #[test]
    fn view_returns_some_when_non_empty() {
        let pal = Palette::dark();
        let embers = vec![make_ember("em-1", "glow")];
        assert!(view(&embers, &pal).is_some());
    }

    #[test]
    fn accents_differ_by_type() {
        let glow = accent_for("glow");
        let flash = accent_for("flash");
        let flare = accent_for("flare");
        let blaze = accent_for("blaze");
        // Sanity: the four primary types must not collapse onto one color.
        assert!(glow != flash);
        assert!(flash != flare);
        assert!(flare != blaze);
        assert!(glow != blaze);
    }

    #[test]
    fn glow_is_blue_flash_is_yellow_flare_is_orange_blaze_is_red() {
        // Lock in the spark-description color convention so future
        // refactors don't silently remap the palette.
        let glow = accent_for("glow");
        assert!(glow.b > glow.r && glow.b > glow.g, "glow should be blue");

        let flash = accent_for("flash");
        assert!(
            flash.r > 0.9 && flash.g > 0.7 && flash.b < 0.4,
            "flash should be yellow"
        );

        let flare = accent_for("flare");
        assert!(
            flare.r > 0.9 && flare.g > 0.4 && flare.g < 0.7 && flare.b < 0.3,
            "flare should be orange"
        );

        let blaze = accent_for("blaze");
        assert!(
            blaze.r > 0.8 && blaze.g < 0.4 && blaze.b < 0.4,
            "blaze should be red"
        );
    }

    #[test]
    fn labels_non_empty_for_all_known_types() {
        for ty in ["glow", "flash", "flare", "blaze", "ash", "unknown"] {
            assert!(!label_for(ty).is_empty());
        }
    }

    #[test]
    fn view_handles_all_four_types_in_single_bar() {
        let pal = Palette::dark();
        let embers = vec![
            make_ember("1", "glow"),
            make_ember("2", "flash"),
            make_ember("3", "flare"),
            make_ember("4", "blaze"),
        ];
        assert!(view(&embers, &pal).is_some());
    }
}
