// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Small bordered pill widgets used wherever sparks are displayed.
//!
//! The spark picker, the workgraph panel, and the Hands/Activity panel
//! all need to label rows with the spark's type (`EPIC`, `TASK`, ...) and
//! priority (`P0`–`P4`). Originally each panel rolled its own copy; this
//! module is the single source of truth so the visual stays consistent.
//!
//! Both helpers are generic over the message type so they slot into any
//! `iced::Element<Message>` without needing per-call mapping.

use iced::widget::{container, text};
use iced::{Element, Theme};

use crate::style::{FONT_LABEL, FONT_SMALL, Palette};

/// Convert a raw spark type string into the short uppercase abbreviation
/// shown inside [`type_badge`]. Kept as a free function (not behind the
/// widget) so callers can reuse the label without rebuilding the widget
/// when they need plain text.
pub fn type_label(spark_type: &str) -> String {
    match spark_type {
        "epic" => "EPIC".to_string(),
        "task" => "TASK".to_string(),
        "bug" => "BUG".to_string(),
        "feature" => "FEAT".to_string(),
        "chore" => "CHORE".to_string(),
        "spike" => "SPIKE".to_string(),
        "milestone" => "MILE".to_string(),
        other => other.to_uppercase().chars().take(4).collect(),
    }
}

/// Bordered uppercase pill labelling a spark's type. Generic over the
/// caller's message type so it slots into any panel.
pub fn type_badge<'a, Message: 'a>(spark_type: &str, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let label = type_label(spark_type);
    container(text(label).size(FONT_SMALL).color(pal.text_secondary))
        .padding([1, 4])
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(pal.surface)),
            border: iced::Border {
                color: pal.border,
                width: 1.0,
                radius: iced::border::Radius::from(4.0),
            },
            ..container::Style::default()
        })
        .into()
}

/// Render a `P{n}` priority chip. Used alongside [`type_badge`] in row
/// layouts that mirror the spark picker. Takes the priority as `i32`
/// to match the column type on `Spark` — callers don't have to widen.
pub fn priority_badge<'a, Message: 'a>(priority: i32, pal: &Palette) -> Element<'a, Message> {
    text(format!("P{priority}"))
        .size(FONT_LABEL)
        .color(pal.text_tertiary)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_label_uppercases_known_types() {
        assert_eq!(type_label("epic"), "EPIC");
        assert_eq!(type_label("task"), "TASK");
        assert_eq!(type_label("bug"), "BUG");
        assert_eq!(type_label("feature"), "FEAT");
        assert_eq!(type_label("chore"), "CHORE");
        assert_eq!(type_label("spike"), "SPIKE");
        assert_eq!(type_label("milestone"), "MILE");
    }

    #[test]
    fn type_label_truncates_unknown_types_to_4_chars() {
        // Defensive: a future spark type we don't know about still gets
        // a sensible 4-char-max badge instead of overflowing the row.
        assert_eq!(type_label("research"), "RESE");
        assert_eq!(type_label("doc"), "DOC");
    }
}
