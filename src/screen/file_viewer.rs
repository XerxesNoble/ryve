// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! File viewer — syntax-highlighted code display with git diff gutter and spark links.
//!
//! Uses viewport-based rendering: only visible lines (plus a small overscan)
//! are materialised as iced widgets. Off-screen content is represented by
//! fixed-height spacers so the scrollbar stays accurate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use data::git::LineChange;
use data::sparks::types::SparkFileLink;
use iced::widget::{container, row, scrollable, text, Space};
use iced::{Color, Element, Font, Length, Theme};
use syntect::highlighting::{self, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::style::Palette;

// ── Shared syntax resources (loaded once) ────────────

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

// ── Messages ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    /// File content loaded from disk (already highlighted).
    FileLoaded {
        tab_id: u64,
        content: String,
        lines: Vec<HighlightedLine>,
        line_changes: HashMap<u32, LineChange>,
        spark_links: Vec<SparkFileLink>,
    },
    /// Navigate to a linked spark.
    GoToSpark(String),
    /// Viewport changed — carries scroll‑y offset and viewport height.
    Scrolled { offset_y: f32, viewport_height: f32 },
}

// ── State ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileViewerState {
    pub path: PathBuf,
    /// Raw file text — kept only for potential future edits / search.
    pub content: String,
    /// Pre-highlighted lines. Empty while the async load is in-flight.
    pub lines: Vec<HighlightedLine>,
    pub line_changes: HashMap<u32, LineChange>,
    pub spark_links: Vec<SparkFileLink>,
    /// Current vertical scroll offset in pixels.
    pub scroll_offset: f32,
    /// Last-known viewport height in pixels (updated on every scroll event).
    pub viewport_height: f32,
}

#[derive(Debug, Clone)]
pub struct HighlightedLine {
    pub spans: Vec<StyledSpan>,
}

#[derive(Debug, Clone)]
pub struct StyledSpan {
    pub text: String,
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
}

impl FileViewerState {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            content: String::new(),
            lines: Vec::new(),
            line_changes: HashMap::new(),
            spark_links: Vec::new(),
            scroll_offset: 0.0,
            viewport_height: 900.0, // reasonable default until first scroll event
        }
    }

    /// Accept pre-highlighted content from the async loader.
    pub fn set_content(
        &mut self,
        content: String,
        lines: Vec<HighlightedLine>,
        line_changes: HashMap<u32, LineChange>,
        spark_links: Vec<SparkFileLink>,
    ) {
        self.content = content;
        self.lines = lines;
        self.line_changes = line_changes;
        self.spark_links = spark_links;
    }

    /// Evict heavy data for a background tab. Preserves path + scroll state.
    pub fn evict(&mut self) {
        self.content = String::new();
        self.lines = Vec::new();
        self.line_changes = HashMap::new();
        self.spark_links = Vec::new();
    }

    /// Whether this viewer has content loaded.
    pub fn is_loaded(&self) -> bool {
        !self.lines.is_empty()
    }

    /// Get spark links that apply to a specific line.
    pub fn spark_links_for_line(&self, line_num: u32) -> Vec<&SparkFileLink> {
        self.spark_links
            .iter()
            .filter(|link| {
                match (link.line_start, link.line_end) {
                    (Some(start), Some(end)) => {
                        line_num >= start as u32 && line_num <= end as u32
                    }
                    (Some(start), None) => line_num == start as u32,
                    // Whole-file link
                    (None, _) => true,
                }
            })
            .collect()
    }
}

// ── Syntax highlighting (runs off the main thread) ───

pub fn highlight_content(content: &str, path: &Path, light_mode: bool) -> Vec<HighlightedLine> {
    let ss = &*SYNTAX_SET;
    let ts = &*THEME_SET;
    let theme_name = if light_mode {
        "InspiredGitHub"
    } else {
        "base16-ocean.dark"
    };
    let theme = &ts.themes[theme_name];

    let syntax = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| ss.find_syntax_by_extension(ext))
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);
    let mut result = Vec::new();

    for line in content.lines() {
        let ranges = highlighter
            .highlight_line(line, ss)
            .unwrap_or_default();

        let spans = ranges
            .into_iter()
            .map(|(style, text)| StyledSpan {
                text: text.to_string(),
                color: syntect_to_iced_color(style.foreground),
                bold: style.font_style.contains(highlighting::FontStyle::BOLD),
                italic: style.font_style.contains(highlighting::FontStyle::ITALIC),
            })
            .collect();

        result.push(HighlightedLine { spans });
    }

    // Handle empty file
    if result.is_empty() {
        result.push(HighlightedLine { spans: Vec::new() });
    }

    result
}

fn syntect_to_iced_color(c: highlighting::Color) -> Color {
    Color::from_rgba8(c.r, c.g, c.b, c.a as f32 / 255.0)
}

// ── View (viewport-culled) ───────────────────────────

const MONO_FONT: Font = Font::MONOSPACE;
const FONT_SIZE: f32 = 13.0;
const LINE_HEIGHT: f32 = 20.0;
const GUTTER_WIDTH: f32 = 16.0;
/// Extra lines rendered above/below the visible window to reduce flicker.
const OVERSCAN: usize = 20;

/// Render the file viewer for a tab. Only the visible slice of lines is
/// materialised as widgets; the rest is represented by spacers.
pub fn view<'a>(state: &'a FileViewerState, pal: &Palette, has_bg: bool) -> Element<'a, Message> {
    if state.lines.is_empty() {
        return container(text("Loading...").size(14).color(pal.text_secondary))
            .center(Length::Fill)
            .into();
    }

    let total_lines = state.lines.len();
    let line_num_chars = total_lines.to_string().len().max(3);

    // ── Compute visible range ──
    let first_visible = (state.scroll_offset / LINE_HEIGHT).floor().max(0.0) as usize;
    let lines_in_viewport =
        (state.viewport_height / LINE_HEIGHT).ceil() as usize + 1;
    let range_start = first_visible.saturating_sub(OVERSCAN);
    let range_end = (first_visible + lines_in_viewport + OVERSCAN).min(total_lines);

    // ── Build spacers + visible rows ──
    let top_pad = range_start as f32 * LINE_HEIGHT;
    let bottom_pad = (total_lines - range_end) as f32 * LINE_HEIGHT;

    let visible_count = range_end - range_start;
    let mut rows: Vec<Element<'a, Message>> = Vec::with_capacity(visible_count + 2);

    // Top spacer (preserves scroll position)
    if top_pad > 0.0 {
        rows.push(Space::new().width(Length::Fill).height(top_pad).into());
    }

    // Only materialise widgets for the visible slice
    for idx in range_start..range_end {
        let line = &state.lines[idx];
        let line_num = (idx + 1) as u32;

        // ── Git gutter indicator ──
        let gutter_element = gutter_indicator(state.line_changes.get(&line_num));

        // ── Line number ──
        let num_str = format!("{:>width$}", line_num, width = line_num_chars);
        let line_num_el = text(num_str)
            .size(FONT_SIZE)
            .font(MONO_FONT)
            .color(pal.text_tertiary);

        // ── Spark link indicator ──
        let spark_links = state.spark_links_for_line(line_num);
        let spark_indicator: Element<'a, Message> = if !spark_links.is_empty() {
            let spark_id = spark_links[0].spark_id.clone();
            iced::widget::button(
                text("\u{26A1}")
                    .size(10.0)
                    .color(Color::from_rgb(1.0, 0.75, 0.2)),
            )
            .style(iced::widget::button::text)
            .padding([0, 2])
            .on_press(Message::GoToSpark(spark_id))
            .into()
        } else {
            Space::new().width(14).into()
        };

        // ── Highlighted code content ──
        let code_element = render_highlighted_line(line);

        // ── Assemble the line row ──
        let line_bg = line_background_color(state.line_changes.get(&line_num), has_bg);

        let line_row = row![
            gutter_element,
            line_num_el,
            Space::new().width(8),
            spark_indicator,
            Space::new().width(4),
            code_element,
        ]
        .spacing(0)
        .align_y(iced::Alignment::Center)
        .height(LINE_HEIGHT);

        let styled_row = container(line_row)
            .width(Length::Fill)
            .padding([0, 8])
            .style(move |_theme: &Theme| container::Style {
                background: line_bg.map(iced::Background::Color),
                ..Default::default()
            });

        rows.push(styled_row.into());
    }

    // Bottom spacer
    if bottom_pad > 0.0 {
        rows.push(Space::new().width(Length::Fill).height(bottom_pad).into());
    }

    let code_column = iced::widget::Column::with_children(rows)
        .spacing(0)
        .width(Length::Fill);

    scrollable(code_column)
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

/// Render the git gutter indicator for a line.
fn gutter_indicator(change: Option<&LineChange>) -> Element<'_, Message> {
    let (symbol, color) = match change {
        Some(LineChange::Added) => ("\u{2502}", Color::from_rgb(0.3, 0.85, 0.4)),    // │ green
        Some(LineChange::Modified) => ("\u{2502}", Color::from_rgb(0.9, 0.8, 0.3)),   // │ yellow
        Some(LineChange::Deleted) => ("\u{25BC}", Color::from_rgb(0.9, 0.35, 0.35)),  // ▼ red
        None => (" ", Color::TRANSPARENT),
    };

    container(
        text(symbol)
            .size(FONT_SIZE)
            .font(MONO_FONT)
            .color(color),
    )
    .width(GUTTER_WIDTH)
    .center_y(LINE_HEIGHT)
    .into()
}

/// Background color for changed lines.
fn line_background_color(change: Option<&LineChange>, _has_bg: bool) -> Option<Color> {
    match change {
        Some(LineChange::Added) => Some(Color::from_rgba(0.2, 0.55, 0.25, 0.12)),
        Some(LineChange::Modified) => Some(Color::from_rgba(0.55, 0.50, 0.15, 0.12)),
        Some(LineChange::Deleted) => Some(Color::from_rgba(0.55, 0.15, 0.15, 0.12)),
        None => None,
    }
}

/// Render a single highlighted line as a row of colored text spans.
fn render_highlighted_line<'a>(line: &'a HighlightedLine) -> Element<'a, Message> {
    if line.spans.is_empty() {
        return Space::new().width(Length::Fill).height(LINE_HEIGHT).into();
    }

    let mut parts: Vec<Element<'a, Message>> = Vec::new();

    for span in &line.spans {
        let mut t = text(&span.text)
            .size(FONT_SIZE)
            .color(span.color);

        if span.bold || span.italic {
            t = t.font(MONO_FONT);
        } else {
            t = t.font(MONO_FONT);
        }

        parts.push(t.into());
    }

    iced::widget::Row::with_children(parts)
        .spacing(0)
        .height(LINE_HEIGHT)
        .into()
}

// ── Async loading ─────────────────────────────────────

/// Load file content, git diff, spark links, and pre-compute syntax highlighting.
/// All heavy work happens off the main thread.
pub async fn load_file(
    tab_id: u64,
    path: PathBuf,
    repo_root: PathBuf,
    pool: Option<sqlx::SqlitePool>,
    workshop_id: String,
    light_mode: bool,
) -> Message {
    // Read file content
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|e| format!("Error reading file: {e}"));

    // Get line-level git diff
    let is_repo = data::git::Repository::is_repo(&repo_root).await;
    let line_changes = if is_repo {
        let repo = data::git::Repository::new(&repo_root);
        repo.line_diff(&path).await.unwrap_or_default()
    } else {
        HashMap::new()
    };

    // Load spark links for this file
    let rel_path = path
        .strip_prefix(&repo_root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();

    let spark_links = if let Some(ref pool) = pool {
        data::sparks::file_link_repo::list_for_file(pool, &rel_path, &workshop_id)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // Syntax highlighting — runs on the blocking tokio thread pool
    let highlight_path = path.clone();
    let highlight_content_str = content.clone();
    let lines = tokio::task::spawn_blocking(move || {
        highlight_content(&highlight_content_str, &highlight_path, light_mode)
    })
    .await
    .unwrap_or_else(|_| vec![HighlightedLine { spans: Vec::new() }]);

    Message::FileLoaded {
        tab_id,
        content,
        lines,
        line_changes,
        spark_links,
    }
}
