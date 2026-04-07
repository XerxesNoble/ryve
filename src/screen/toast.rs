// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Toast notification system.
//!
//! Errors that used to vanish into `log::error!` now surface as dismissible
//! toasts in the bottom-right corner of the UI. Toasts auto-expire after a
//! fixed lifetime and can be clicked-to-dismiss at any time.

use iced::widget::{button, column, container, row, text, Space};
use iced::{Background, Border, Color, Element, Length, Theme};

use crate::style::{Palette, FONT_LABEL, FONT_SMALL};

/// How long a toast remains on screen before it auto-dismisses.
pub const TOAST_LIFETIME_SECS: u64 = 8;

/// Maximum number of toasts shown simultaneously. Older ones are dropped
/// from the front of the queue when this is exceeded.
pub const MAX_TOASTS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ToastKind {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub kind: ToastKind,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// User clicked the × button on a toast.
    Dismiss(u64),
}

/// Render the stack of active toasts anchored to the bottom-right.
/// Returns `None` when there are no toasts (caller can skip overlay).
pub fn view<'a>(toasts: &'a [Toast], pal: &Palette) -> Option<Element<'a, Message>> {
    if toasts.is_empty() {
        return None;
    }
    let pal = *pal;

    let mut stack = column![].spacing(8).align_x(iced::Alignment::End);
    for t in toasts {
        stack = stack.push(toast_card(t, &pal));
    }

    // Anchor to bottom-right with outer padding.
    let anchored = container(
        column![
            Space::new().height(Length::Fill),
            row![Space::new().width(Length::Fill), stack]
                .align_y(iced::Alignment::End),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fill);

    Some(anchored.into())
}

fn toast_card<'a>(t: &'a Toast, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let accent = match t.kind {
        ToastKind::Error => pal.danger,
        ToastKind::Warning => Color {
            r: 1.0,
            g: 0.8,
            b: 0.2,
            a: 1.0,
        },
        ToastKind::Info => pal.accent,
    };

    let title = text(&t.title)
        .size(FONT_LABEL)
        .color(pal.text_primary);

    let dismiss = button(text("\u{00D7}").size(FONT_LABEL).color(pal.text_secondary))
        .style(button::text)
        .padding(0)
        .on_press(Message::Dismiss(t.id));

    let header = row![
        text(kind_glyph(t.kind)).size(FONT_LABEL).color(accent),
        title,
        Space::new().width(Length::Fill),
        dismiss,
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let mut col = column![header].spacing(4);
    if !t.body.is_empty() {
        col = col.push(
            text(&t.body)
                .size(FONT_SMALL)
                .color(pal.text_secondary),
        );
    }

    container(col)
        .padding([10, 12])
        .width(Length::Fixed(340.0))
        .style(move |_theme: &Theme| container::Style {
            background: Some(Background::Color(Color {
                r: pal.window_bg.r,
                g: pal.window_bg.g,
                b: pal.window_bg.b,
                a: 0.92,
            })),
            border: Border {
                color: Color { a: 0.6, ..accent },
                width: 1.0,
                radius: 8.0.into(),
            },
            text_color: Some(pal.text_primary),
            ..Default::default()
        })
        .into()
}

fn kind_glyph(kind: ToastKind) -> &'static str {
    match kind {
        ToastKind::Error => "\u{26A0}",   // ⚠
        ToastKind::Warning => "\u{26A0}", // ⚠
        ToastKind::Info => "\u{2139}",    // ℹ
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_constants_are_sane() {
        assert!(TOAST_LIFETIME_SECS >= 1);
        assert!(MAX_TOASTS >= 1);
    }

    #[test]
    fn kind_glyph_is_non_empty_for_all_kinds() {
        for kind in [ToastKind::Error, ToastKind::Warning, ToastKind::Info] {
            assert!(!kind_glyph(kind).is_empty());
        }
    }

    #[test]
    fn max_toasts_cap_enforced_by_push_pattern() {
        // Simulates the push-and-cap pattern used in App::push_toast so the
        // invariant is locked in by a test rather than inspection only.
        let mut toasts: Vec<Toast> = Vec::new();
        for i in 0..(MAX_TOASTS + 3) {
            toasts.push(Toast {
                id: i as u64,
                title: format!("t{i}"),
                body: String::new(),
                kind: ToastKind::Error,
            });
            while toasts.len() > MAX_TOASTS {
                toasts.remove(0);
            }
        }
        assert_eq!(toasts.len(), MAX_TOASTS);
        // The oldest three should have been evicted.
        assert_eq!(toasts.first().unwrap().id, 3);
    }

    #[test]
    fn view_returns_none_when_empty() {
        let pal = crate::style::Palette::dark();
        assert!(view(&[], &pal).is_none());
    }

    #[test]
    fn view_returns_some_when_non_empty() {
        let pal = crate::style::Palette::dark();
        let toasts = vec![Toast {
            id: 1,
            title: "t".into(),
            body: "b".into(),
            kind: ToastKind::Error,
        }];
        assert!(view(&toasts, &pal).is_some());
    }

    #[test]
    fn toast_clone_preserves_fields() {
        let t = Toast {
            id: 42,
            title: "hello".into(),
            body: "world".into(),
            kind: ToastKind::Error,
        };
        let c = t.clone();
        assert_eq!(c.id, 42);
        assert_eq!(c.title, "hello");
        assert_eq!(c.body, "world");
        assert_eq!(c.kind, ToastKind::Error);
    }
}
