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

use std::path::{Path, PathBuf};

use iced::widget::{column, scrollable, text};
use iced::{Element, Font, Length};

use crate::style::{FONT_BODY, FONT_LABEL, Palette};

/// Number of trailing bytes of the log file to keep in memory. Logs from
/// long-running Hands can grow indefinitely, so we cap the tail at ~64KiB
/// — enough to see recent agent thinking without bloating the UI.
const TAIL_BYTES: u64 = 64 * 1024;

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
}

#[derive(Debug, Clone)]
pub struct LogTailState {
    pub path: PathBuf,
    pub content: String,
    /// Set to a human-readable error when the most recent load failed.
    pub error: Option<String>,
}

impl LogTailState {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            content: String::new(),
            error: None,
        }
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
    } else if state.content.is_empty() {
        text("(no output yet — the Hand has not written to its log)")
            .size(FONT_BODY)
            .color(pal.text_tertiary)
            .into()
    } else {
        text(&state.content)
            .size(FONT_BODY)
            .font(Font::MONOSPACE)
            .color(pal.text_primary)
            .into()
    };

    let content = column![header, body].spacing(8).padding(12);

    scrollable(content)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
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
}
