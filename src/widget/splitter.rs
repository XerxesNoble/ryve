// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Drag-to-resize splitter handles for the workshop layout.
//!
//! A splitter is a thin (`PANEL_GAP`-sized) hit zone that sits between
//! two panels and lets the user drag to resize them. The widget itself
//! only emits an `on_press` message; the actual drag tracking is done
//! at the application level via a global mouse-event subscription that
//! is only active while a drag is in progress.
//!
//! There are two splitter orientations:
//!
//! - **Vertical** — a tall, thin handle for resizing horizontally
//!   adjacent panels (sidebar ↔ bench, bench ↔ sparks). The mouse
//!   cursor changes to `ResizingHorizontally` on hover.
//! - **Horizontal** — a wide, short handle for resizing vertically
//!   stacked panels (files ↕ hands inside the sidebar). The mouse
//!   cursor changes to `ResizingVertically`.

use iced::widget::{Space, container, mouse_area};
use iced::{Background, Color, Element, Length, Theme, mouse};

use crate::style::{PANEL_GAP, Palette};

/// Identifies which splitter the user grabbed.
///
/// The orientation and which config field to mutate are both encoded
/// here so the application's update loop has a single source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitterKind {
    /// Right edge of the left sidebar — drives `layout.sidebar_width`.
    SidebarRight,
    /// Left edge of the sparks panel — drives `layout.sparks_width`.
    SparksLeft,
    /// Horizontal divider between files & hands inside the sidebar —
    /// drives `layout.sidebar_split` (a 0..1 ratio).
    SidebarFilesHands,
}

impl SplitterKind {
    /// Whether the drag should be tracked along the X axis (vertical
    /// handle dragged left/right) or the Y axis (horizontal handle).
    pub fn is_horizontal_drag(self) -> bool {
        matches!(self, Self::SidebarRight | Self::SparksLeft)
    }
}

fn handle_style(pal: Palette) -> impl Fn(&Theme) -> container::Style {
    move |_theme: &Theme| container::Style {
        background: Some(Background::Color(Color {
            r: pal.text_secondary.r,
            g: pal.text_secondary.g,
            b: pal.text_secondary.b,
            a: 0.0, // invisible by default — the gap is enough
        })),
        ..Default::default()
    }
}

/// A vertical drag handle for resizing two horizontally-adjacent
/// panels. The width matches `PANEL_GAP` so it visually replaces the
/// row spacing it removes.
pub fn vertical<'a, Message>(on_press: Message, pal: &Palette) -> Element<'a, Message>
where
    Message: 'a + Clone,
{
    let bar = container(Space::new())
        .width(Length::Fixed(PANEL_GAP))
        .height(Length::Fill)
        .style(handle_style(*pal));

    mouse_area(bar)
        .interaction(mouse::Interaction::ResizingHorizontally)
        .on_press(on_press)
        .into()
}

/// A horizontal drag handle for resizing two vertically-stacked
/// panels. The height matches `PANEL_GAP`.
pub fn horizontal<'a, Message>(on_press: Message, pal: &Palette) -> Element<'a, Message>
where
    Message: 'a + Clone,
{
    let bar = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(PANEL_GAP))
        .style(handle_style(*pal));

    mouse_area(bar)
        .interaction(mouse::Interaction::ResizingVertically)
        .on_press(on_press)
        .into()
}

// ── Drag-state helpers ───────────────────────────────────────────────

/// Bounds applied to splitter values so panels can never collapse to
/// nothing or push their neighbours off-screen.
pub const MIN_PANEL_WIDTH: f32 = 160.0;
pub const MAX_PANEL_WIDTH: f32 = 800.0;
pub const MIN_SPLIT_RATIO: f32 = 0.15;
pub const MAX_SPLIT_RATIO: f32 = 0.85;

/// In-flight drag state owned by the application.
///
/// `start_cursor` is captured on the first `CursorMoved` event after
/// `on_press`, and `start_value` is the original config value at the
/// moment the drag began. Subsequent move events compute new values as
/// `start_value + delta` (or `start_value - delta` for handles whose
/// "grow" direction is reversed).
#[derive(Debug, Clone, Copy)]
pub struct SplitterDrag {
    pub kind: SplitterKind,
    pub start_cursor: Option<f32>,
    pub start_value: f32,
}

impl SplitterDrag {
    pub fn new(kind: SplitterKind, start_value: f32) -> Self {
        Self {
            kind,
            start_cursor: None,
            start_value,
        }
    }
}

/// Compute the new panel width / split ratio for a drag, given the
/// current cursor coordinate along the drag axis and the height of the
/// sidebar (used only for the sidebar files↕hands ratio).
///
/// Returns `None` if the drag is in its capture phase (no start cursor
/// recorded yet); the caller should record the cursor and try again on
/// the next move event.
pub fn compute_new_value(drag: &SplitterDrag, cursor: f32, sidebar_height: f32) -> f32 {
    let start = drag.start_cursor.unwrap_or(cursor);
    let delta = cursor - start;
    match drag.kind {
        SplitterKind::SidebarRight => {
            (drag.start_value + delta).clamp(MIN_PANEL_WIDTH, MAX_PANEL_WIDTH)
        }
        SplitterKind::SparksLeft => {
            // Sparks panel grows when the handle is dragged LEFT, so
            // the delta is subtracted.
            (drag.start_value - delta).clamp(MIN_PANEL_WIDTH, MAX_PANEL_WIDTH)
        }
        SplitterKind::SidebarFilesHands => {
            let height = sidebar_height.max(1.0);
            (drag.start_value + delta / height).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_right_grows_right() {
        let mut drag = SplitterDrag::new(SplitterKind::SidebarRight, 250.0);
        drag.start_cursor = Some(300.0);
        // Cursor moved 50px to the right → width grows by 50.
        assert_eq!(compute_new_value(&drag, 350.0, 0.0), 300.0);
        // And 80px to the left → width shrinks by 80.
        assert_eq!(compute_new_value(&drag, 220.0, 0.0), 250.0 - 80.0);
    }

    #[test]
    fn sparks_left_grows_left() {
        let mut drag = SplitterDrag::new(SplitterKind::SparksLeft, 280.0);
        drag.start_cursor = Some(1000.0);
        // Cursor moved LEFT by 60px → sparks panel grows by 60.
        assert_eq!(compute_new_value(&drag, 940.0, 0.0), 340.0);
        // Cursor moved RIGHT by 40px → sparks panel shrinks by 40.
        assert_eq!(compute_new_value(&drag, 1040.0, 0.0), 240.0);
    }

    #[test]
    fn sidebar_split_uses_height_ratio() {
        let mut drag = SplitterDrag::new(SplitterKind::SidebarFilesHands, 0.5);
        drag.start_cursor = Some(400.0);
        // Cursor moved down 100px in an 800px-tall sidebar → +0.125 ratio.
        let v = compute_new_value(&drag, 500.0, 800.0);
        assert!((v - 0.625).abs() < 1e-5);
    }

    #[test]
    fn widths_are_clamped() {
        let mut drag = SplitterDrag::new(SplitterKind::SidebarRight, 250.0);
        drag.start_cursor = Some(0.0);
        assert_eq!(compute_new_value(&drag, -9999.0, 0.0), MIN_PANEL_WIDTH);
        assert_eq!(compute_new_value(&drag, 9999.0, 0.0), MAX_PANEL_WIDTH);
    }

    #[test]
    fn split_ratio_is_clamped() {
        let mut drag = SplitterDrag::new(SplitterKind::SidebarFilesHands, 0.5);
        drag.start_cursor = Some(0.0);
        let too_high = compute_new_value(&drag, 9999.0, 800.0);
        let too_low = compute_new_value(&drag, -9999.0, 800.0);
        assert_eq!(too_high, MAX_SPLIT_RATIO);
        assert_eq!(too_low, MIN_SPLIT_RATIO);
    }

    #[test]
    fn first_move_captures_start_cursor() {
        let drag = SplitterDrag::new(SplitterKind::SidebarRight, 250.0);
        // No start cursor yet → first sample treated as the anchor.
        assert_eq!(compute_new_value(&drag, 1234.0, 0.0), 250.0);
    }
}
