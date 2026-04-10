// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Read-only spy view for background Hands.
//!
//! CLI-spawned Hands run as detached subprocesses with no terminal tab —
//! their stdout/stderr is redirected to a log file under `.ryve/logs/`.
//! This module renders the tail of that log file as a scrollable monospace
//! text view so the user can see what a background Hand is doing without
//! attaching to it. Spark ryve-8c14734a.
//!
//! The view auto-refreshes whenever the parent app reloads it (currently
//! on every `SparksPoll` tick when this tab is active).
//!
//! Rendering is virtualized: only the lines visible inside the viewport are
//! turned into `text()` widgets. A top/bottom spacer fakes the total height
//! so the scrollbar behaves correctly. This keeps frame time constant
//! regardless of log buffer size (spark ryve-6780f6e7).

use std::path::{Path, PathBuf};

use iced::widget::{Space, column, scrollable, text};
use iced::{Element, Font, Length};

use crate::style::{FONT_BODY, FONT_LABEL, Palette};

/// Number of trailing bytes of the log file to keep in memory. Logs from
/// long-running Hands can grow indefinitely, so we cap the tail at ~64KiB
/// — enough to see recent agent thinking without bloating the UI.
const TAIL_BYTES: u64 = 64 * 1024;

use perf_core::{LOG_LINE_HEIGHT, log_tail_visible_range};

#[derive(Debug, Clone)]
pub enum Message {
    /// A `load_tail` task finished. The bytes are the tail of the file at
    /// the moment of the read; the receiver replaces the tab's content.
    Loaded {
        tab_id: u64,
        path: PathBuf,
        content: String,
    },
    /// The log file could not be read (does not exist yet, permissions,
    /// etc.). The receiver displays the error inline.
    LoadFailed {
        tab_id: u64,
        path: PathBuf,
        error: String,
    },
    /// The scrollable viewport changed — we store the offset so we can
    /// compute which lines are visible on the next frame.
    Scrolled { offset_y: f32, viewport_height: f32 },
}

#[derive(Debug, Clone)]
pub struct LogTailState {
    pub path: PathBuf,
    /// Pre-split lines from the log tail. Kept in sync with `content` so
    /// we don't re-split on every frame.
    pub lines: Vec<String>,
    /// Set to a human-readable error when the most recent load failed.
    pub error: Option<String>,
    /// Current vertical scroll offset in logical pixels.
    pub scroll_offset_y: f32,
    /// Current viewport height in logical pixels.
    pub viewport_height: f32,
}

impl LogTailState {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lines: Vec::new(),
            error: None,
            scroll_offset_y: 0.0,
            viewport_height: 600.0,
        }
    }

    /// Replace the log content, pre-splitting into lines.
    pub fn set_content(&mut self, content: String) {
        self.lines = content.lines().map(String::from).collect();
        self.error = None;
    }
}

/// Render the spy view for a background Hand.
pub fn view<'a>(state: &'a LogTailState, pal: &Palette) -> Element<'a, Message> {
    let header = text(format!("Spying on {}", state.path.display()))
        .size(FONT_LABEL)
        .color(pal.text_secondary);

    let body: Element<'a, Message> = if let Some(err) = &state.error {
        text(format!("Could not read log file: {err}"))
            .size(FONT_BODY)
            .color(pal.danger)
            .into()
    } else if state.lines.is_empty() {
        text("(no output yet — the Hand has not written to its log)")
            .size(FONT_BODY)
            .color(pal.text_tertiary)
            .into()
    } else {
        virtualized_lines(state, pal)
    };

    let content = column![header, body].spacing(8).padding(12);

    scrollable(content)
        .height(Length::Fill)
        .width(Length::Fill)
        .on_scroll(|viewport| {
            let offset = viewport.absolute_offset();
            Message::Scrolled {
                offset_y: offset.y,
                viewport_height: viewport.bounds().height,
            }
        })
        .into()
}

/// Build a column containing only the lines visible in the current viewport,
/// bookended by spacers that represent the invisible portion so the
/// scrollbar reflects the true content height.
fn virtualized_lines<'a>(state: &'a LogTailState, pal: &Palette) -> Element<'a, Message> {
    let total_lines = state.lines.len();
    let total_height = total_lines as f32 * LOG_LINE_HEIGHT;

    let (first_visible, last_visible) =
        log_tail_visible_range(state.scroll_offset_y, state.viewport_height, total_lines);

    let top_spacer_h = first_visible as f32 * LOG_LINE_HEIGHT;
    let bottom_spacer_h = (total_height - last_visible as f32 * LOG_LINE_HEIGHT).max(0.0);

    let mut col = column![].spacing(0);

    if top_spacer_h > 0.0 {
        col = col.push(Space::new().width(Length::Fill).height(top_spacer_h));
    }

    for line in &state.lines[first_visible..last_visible] {
        col = col.push(
            text(line)
                .size(FONT_BODY)
                .font(Font::MONOSPACE)
                .color(pal.text_primary),
        );
    }

    if bottom_spacer_h > 0.0 {
        col = col.push(Space::new().width(Length::Fill).height(bottom_spacer_h));
    }

    col.into()
}

/// Read the tail of a log file. Runs on the tokio runtime; intended to be
/// driven via `Task::perform`.
pub async fn load_tail(tab_id: u64, path: PathBuf) -> Message {
    match read_tail(&path, TAIL_BYTES).await {
        Ok(content) => Message::Loaded {
            tab_id,
            path,
            content,
        },
        Err(e) => Message::LoadFailed {
            tab_id,
            path,
            error: e.to_string(),
        },
    }
}

/// Read at most `max_bytes` from the end of `path`. Returns the trailing
/// slice as a UTF-8 string (lossy if the cut lands mid-codepoint).
async fn read_tail(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

    let mut file = tokio::fs::File::open(path).await?;
    let metadata = file.metadata().await?;
    let len = metadata.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).await?;
    let mut buf = Vec::with_capacity(max_bytes as usize);
    file.read_to_end(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_tail_returns_full_file_when_smaller_than_cap() {
        let dir = std::env::temp_dir().join(format!("ryve-tail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log");
        std::fs::write(&path, "hello world").unwrap();
        let out = read_tail(&path, 1024).await.unwrap();
        assert_eq!(out, "hello world");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_tail_caps_to_last_n_bytes() {
        let dir = std::env::temp_dir().join(format!("ryve-tail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log");
        // 100 'a's then "TAIL"
        let mut content = "a".repeat(100);
        content.push_str("TAIL");
        std::fs::write(&path, &content).unwrap();
        let out = read_tail(&path, 4).await.unwrap();
        assert_eq!(out, "TAIL");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn load_tail_reports_missing_file_as_load_failed() {
        let path = std::env::temp_dir().join(format!("ryve-missing-{}", uuid::Uuid::new_v4()));
        let msg = load_tail(99, path.clone()).await;
        match msg {
            Message::LoadFailed { tab_id, .. } => assert_eq!(tab_id, 99),
            other => panic!("expected LoadFailed, got {other:?}"),
        }
    }

    #[test]
    fn visible_range_empty() {
        assert_eq!(log_tail_visible_range(0.0, 600.0, 0), (0, 0));
    }

    #[test]
    fn visible_range_small_content() {
        // 10 lines, viewport can show 30 lines — should show all 10
        let (first, last) = log_tail_visible_range(0.0, 600.0, 10);
        assert_eq!(first, 0);
        assert_eq!(last, 10);
    }

    #[test]
    fn visible_range_scrolled_middle() {
        // 1000 lines, viewport shows ~30 lines, scrolled to middle
        let offset = 500.0 * LOG_LINE_HEIGHT; // scrolled to line 500
        let (first, last) = log_tail_visible_range(offset, 600.0, 1000);
        // first should be around 500 - OVERSCAN
        assert!(first <= 500);
        assert!(first >= 490);
        // last should be around 500 + 30 + OVERSCAN
        assert!(last >= 530);
        assert!(last <= 545);
    }

    #[test]
    fn visible_range_at_end() {
        let total = 1000;
        let offset = (total as f32 - 30.0) * LOG_LINE_HEIGHT;
        let (_, last) = log_tail_visible_range(offset, 600.0, total);
        assert_eq!(last, total);
    }

    #[test]
    fn set_content_splits_lines() {
        let mut state = LogTailState::new(PathBuf::from("/tmp/test.log"));
        state.set_content("line1\nline2\nline3".to_string());
        assert_eq!(state.lines.len(), 3);
        assert_eq!(state.lines[0], "line1");
        assert_eq!(state.lines[2], "line3");
    }

    #[test]
    fn virtualization_renders_constant_widgets() {
        // With 100 lines or 100_000 lines, the number of rendered widgets
        // should be bounded by viewport_height / LOG_LINE_HEIGHT + 2*OVERSCAN.
        let max_rendered =
            (600.0_f32 / LOG_LINE_HEIGHT).ceil() as usize + 2 * perf_core::LOG_OVERSCAN_LINES;

        for total in [100, 1_000, 10_000, 100_000] {
            let (first, last) = log_tail_visible_range(0.0, 600.0, total);
            let rendered = last - first;
            assert!(
                rendered <= max_rendered,
                "total={total}: rendered {rendered} > max {max_rendered}"
            );
        }
    }
}
