// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! File viewer — syntax-highlighted code display with git diff gutter and spark links.
//!
//! Uses viewport-based rendering: only visible lines (plus a small overscan)
//! are materialised as iced widgets. Off-screen content is represented by
//! fixed-height spacers so the scrollbar stays accurate.
//!
//! Supports line-level text selection: click a line to select it, shift-click
//! to extend the selection, Cmd+C to copy selected lines.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use data::git::LineChange;
use data::sparks::types::SparkFileLink;
use iced::widget::{Space, button, column, container, mouse_area, row, scrollable, text};
use iced::{Color, Element, Font, Length, Theme};
use syntect::highlighting::{self, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::style::Palette;

// ── Shared syntax resources (loaded once) ────────────

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// Background color for selected lines — semi-transparent blue.
const SELECTION_BG: Color = Color {
    r: 0.20,
    g: 0.45,
    b: 0.80,
    a: 0.28,
};

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
    /// File could not be read from disk.
    FileLoadFailed {
        tab_id: u64,
        path: PathBuf,
        error: String,
    },
    /// Navigate to a linked spark.
    GoToSpark(String),
    /// Viewport changed — carries scroll-y offset and viewport height.
    Scrolled { offset_y: f32, viewport_height: f32 },
    /// User clicked a line (0-indexed). Sets selection anchor or extends if shift held.
    ClickLine(usize),
    /// Copy selected lines to clipboard (Cmd+C).
    CopySelection,
    /// Clear the current selection (Escape or click with no shift on already-selected).
    ClearSelection,
    /// Navigate the file explorer to the given directory (clicked breadcrumb
    /// segment). The path is absolute. Passing the workshop root collapses
    /// the explorer selection back to the root.
    NavigateToDir(PathBuf),
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
    /// Selection anchor line (0-indexed). Set on first click.
    pub selection_anchor: Option<usize>,
    /// Selection end line (0-indexed). Set on shift-click or same as anchor.
    pub selection_end: Option<usize>,
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
            selection_anchor: None,
            selection_end: None,
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

    /// Returns the selected line range as (start, end) inclusive, 0-indexed.
    /// Returns `None` if no selection is active.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        match (self.selection_anchor, self.selection_end) {
            (Some(a), Some(b)) => Some((a.min(b), a.max(b))),
            (Some(a), None) => Some((a, a)),
            _ => None,
        }
    }

    /// Returns true if the given 0-indexed line is within the current selection.
    pub fn is_line_selected(&self, idx: usize) -> bool {
        self.selection_range()
            .is_some_and(|(start, end)| idx >= start && idx <= end)
    }

    /// Extract the text of the selected lines, joined by newlines.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let lines: Vec<&str> = self
            .content
            .lines()
            .enumerate()
            .filter(|(i, _)| *i >= start && *i <= end)
            .map(|(_, line)| line)
            .collect();
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// Clear the current selection.
    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.selection_end = None;
    }

    /// Total number of lines in the loaded file (0 if not yet loaded).
    pub fn total_lines(&self) -> usize {
        self.lines.len()
    }

    /// Best-effort cursor position (1-indexed line and column) for the
    /// status bar. The viewer doesn't have a true text caret, so we use
    /// the active selection anchor; when nothing is selected we report
    /// position (1, 1).
    pub fn cursor_position(&self) -> (usize, usize) {
        let line = self.selection_anchor.map(|i| i + 1).unwrap_or(1);
        // Selections are line-granular, so column is always the start of
        // the line. This is honest given the viewer's capabilities.
        (line, 1)
    }

    /// Get spark links that apply to a specific line.
    pub fn spark_links_for_line(&self, line_num: u32) -> Vec<&SparkFileLink> {
        self.spark_links
            .iter()
            .filter(|link| {
                match (link.line_start, link.line_end) {
                    (Some(start), Some(end)) => line_num >= start as u32 && line_num <= end as u32,
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
        let ranges = highlighter.highlight_line(line, ss).unwrap_or_default();

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

/// Human-readable language label for a file path, used by the status bar.
///
/// Resolves via syntect's syntax set first (to match the highlighter), then
/// falls back to a small extension table for files syntect doesn't know.
/// Returns `"Plain Text"` when no match can be found.
pub fn language_label(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // Common shortcuts — keeps the label stable regardless of which
    // syntect bundle is loaded.
    match ext.as_str() {
        "rs" => return "Rust",
        "py" | "pyi" => return "Python",
        "ts" => return "TypeScript",
        "tsx" => return "TSX",
        "js" | "mjs" | "cjs" => return "JavaScript",
        "jsx" => return "JSX",
        "go" => return "Go",
        "java" => return "Java",
        "kt" | "kts" => return "Kotlin",
        "swift" => return "Swift",
        "c" => return "C",
        "h" => return "C Header",
        "cpp" | "cc" | "cxx" | "hpp" => return "C++",
        "cs" => return "C#",
        "rb" => return "Ruby",
        "php" => return "PHP",
        "json" => return "JSON",
        "yaml" | "yml" => return "YAML",
        "toml" => return "TOML",
        "md" | "markdown" => return "Markdown",
        "html" | "htm" => return "HTML",
        "css" => return "CSS",
        "scss" | "sass" => return "SCSS",
        "sh" | "bash" | "zsh" => return "Shell",
        "sql" => return "SQL",
        "lua" => return "Lua",
        "ex" | "exs" => return "Elixir",
        "txt" => return "Plain Text",
        _ => {}
    }

    // Fall back to syntect, but only return a static label.
    if !ext.is_empty() && SYNTAX_SET.find_syntax_by_extension(&ext).is_some() {
        // syntect knew about it but we don't have a friendly name; show
        // the extension uppercased via a single static label set.
        return "Source";
    }

    "Plain Text"
}

// ── Breadcrumb path ──────────────────────────────────

/// One segment of the breadcrumb path. The label is what gets rendered;
/// `path` is the absolute filesystem path that segment refers to (used as
/// the navigation target when the segment is clicked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreadcrumbSegment {
    pub label: String,
    pub path: PathBuf,
    /// True for the final segment (the file itself), which should not be
    /// rendered as an interactive button.
    pub is_file: bool,
}

/// Build the breadcrumb segments from the workshop root down to the file.
///
/// The first segment is the workshop root (using its directory name as the
/// label, falling back to the full path string when the root has no file
/// name component). Each intermediate segment is a parent directory, and
/// the final segment is the file itself. When `file_path` is not under
/// `workshop_root` the function falls back to the file's own ancestors so
/// the user still gets a sensible breadcrumb.
pub fn breadcrumb_segments(workshop_root: &Path, file_path: &Path) -> Vec<BreadcrumbSegment> {
    let root_label = workshop_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| workshop_root.to_string_lossy().into_owned());

    let mut segments = vec![BreadcrumbSegment {
        label: root_label,
        path: workshop_root.to_path_buf(),
        is_file: false,
    }];

    let rel = match file_path.strip_prefix(workshop_root) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => return segments,
    };

    let components: Vec<_> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(os) => Some(os.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    let mut cumulative = workshop_root.to_path_buf();
    let last = components.len().saturating_sub(1);
    for (i, name) in components.iter().enumerate() {
        cumulative.push(name);
        segments.push(BreadcrumbSegment {
            label: name.clone(),
            path: cumulative.clone(),
            is_file: i == last,
        });
    }

    segments
}

// ── View (viewport-culled) ───────────────────────────

const MONO_FONT: Font = Font::MONOSPACE;
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 22.0;
const GUTTER_WIDTH: f32 = 16.0;
const BREADCRUMB_SIZE: f32 = 12.0;
/// Extra lines rendered above/below the visible window to reduce flicker.
const OVERSCAN: usize = 20;

/// Render the breadcrumb bar shown above the file content. Each directory
/// segment is a clickable button that navigates the file explorer to that
/// directory; the final file segment is rendered as plain text.
fn breadcrumb_bar<'a>(
    workshop_root: &Path,
    file_path: &Path,
    pal: &Palette,
) -> Element<'a, Message> {
    let segments = breadcrumb_segments(workshop_root, file_path);
    let mut row_items: Vec<Element<'a, Message>> = Vec::with_capacity(segments.len() * 2);

    for (i, seg) in segments.into_iter().enumerate() {
        if i > 0 {
            row_items.push(
                text(" \u{203A} ")
                    .size(BREADCRUMB_SIZE)
                    .color(pal.text_tertiary)
                    .into(),
            );
        }

        if seg.is_file {
            row_items.push(
                text(seg.label)
                    .size(BREADCRUMB_SIZE)
                    .color(pal.text_primary)
                    .into(),
            );
        } else {
            let label = text(seg.label)
                .size(BREADCRUMB_SIZE)
                .color(pal.text_secondary);
            row_items.push(
                button(label)
                    .style(button::text)
                    .padding([0, 2])
                    .on_press(Message::NavigateToDir(seg.path))
                    .into(),
            );
        }
    }

    container(
        iced::widget::Row::with_children(row_items)
            .spacing(0)
            .align_y(iced::Alignment::Center),
    )
    .padding([4, 10])
    .width(Length::Fill)
    .into()
}

/// Render the file viewer for a tab. Only the visible slice of lines is
/// materialised as widgets; the rest is represented by spacers.
pub fn view<'a>(
    state: &'a FileViewerState,
    workshop_root: &Path,
    pal: &Palette,
    has_bg: bool,
) -> Element<'a, Message> {
    let breadcrumb = breadcrumb_bar(workshop_root, &state.path, pal);

    if state.lines.is_empty() {
        let loading = container(text("Loading...").size(14).color(pal.text_secondary))
            .center(Length::Fill);
        return column![breadcrumb, loading]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }

    let total_lines = state.lines.len();
    let line_num_chars = total_lines.to_string().len().max(3);

    // ── Compute visible range ──
    let first_visible = (state.scroll_offset / LINE_HEIGHT).floor().max(0.0) as usize;
    let lines_in_viewport = (state.viewport_height / LINE_HEIGHT).ceil() as usize + 1;
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
                    .size(12.0)
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
        // Selection highlight takes priority over git diff background.
        let is_selected = state.is_line_selected(idx);
        let line_bg = if is_selected {
            Some(SELECTION_BG)
        } else {
            line_background_color(state.line_changes.get(&line_num), has_bg)
        };

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

        // Wrap in mouse_area for line selection via click.
        // Shift state is tracked globally; main.rs decides whether to
        // set anchor or extend the selection range.
        let clickable_row = mouse_area(styled_row).on_press(Message::ClickLine(idx));

        rows.push(clickable_row.into());
    }

    // Bottom spacer
    if bottom_pad > 0.0 {
        rows.push(Space::new().width(Length::Fill).height(bottom_pad).into());
    }

    let code_column = iced::widget::Column::with_children(rows)
        .spacing(0)
        .width(Length::Fill);

    let scroll = scrollable(code_column)
        .height(Length::Fill)
        .width(Length::Fill)
        .on_scroll(|viewport| {
            let offset = viewport.absolute_offset();
            Message::Scrolled {
                offset_y: offset.y,
                viewport_height: viewport.bounds().height,
            }
        });

    column![breadcrumb, scroll]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Render the git gutter indicator for a line.
fn gutter_indicator(change: Option<&LineChange>) -> Element<'_, Message> {
    let (symbol, color) = match change {
        Some(LineChange::Added) => ("\u{2502}", Color::from_rgb(0.3, 0.85, 0.4)), // green
        Some(LineChange::Modified) => ("\u{2502}", Color::from_rgb(0.9, 0.8, 0.3)), // yellow
        Some(LineChange::Deleted) => ("\u{25BC}", Color::from_rgb(0.9, 0.35, 0.35)), // red
        None => (" ", Color::TRANSPARENT),
    };

    container(text(symbol).size(FONT_SIZE).font(MONO_FONT).color(color))
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
        let mut t = text(&span.text).size(FONT_SIZE).color(span.color);

        let font = if span.bold {
            Font {
                weight: iced::font::Weight::Bold,
                ..MONO_FONT
            }
        } else if span.italic {
            Font {
                style: iced::font::Style::Italic,
                ..MONO_FONT
            }
        } else {
            MONO_FONT
        };
        t = t.font(font);

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
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => {
            return Message::FileLoadFailed {
                tab_id,
                path,
                error: e.to_string(),
            };
        }
    };

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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn language_label_known_extensions() {
        assert_eq!(language_label(Path::new("foo.rs")), "Rust");
        assert_eq!(language_label(Path::new("a/b/foo.py")), "Python");
        assert_eq!(language_label(Path::new("foo.TSX")), "TSX");
        assert_eq!(language_label(Path::new("README.md")), "Markdown");
        assert_eq!(language_label(Path::new("Cargo.toml")), "TOML");
    }

    #[test]
    fn language_label_unknown_falls_back() {
        // Some extension we don't list and syntect doesn't have either.
        assert_eq!(language_label(Path::new("foo.zzznope")), "Plain Text");
        // No extension at all.
        assert_eq!(language_label(Path::new("Makefile")), "Plain Text");
    }

    #[test]
    fn cursor_position_defaults_to_one_one() {
        let state = FileViewerState::new(PathBuf::from("foo.rs"));
        assert_eq!(state.cursor_position(), (1, 1));
        assert_eq!(state.total_lines(), 0);
    }

    #[test]
    fn cursor_position_uses_selection_anchor() {
        let mut state = FileViewerState::new(PathBuf::from("foo.rs"));
        state.selection_anchor = Some(11); // 0-indexed line 11
        state.selection_end = Some(15);
        assert_eq!(state.cursor_position(), (12, 1));
    }

    #[test]
    fn breadcrumb_segments_within_workshop() {
        let root = PathBuf::from("/home/dev/workshop");
        let file = PathBuf::from("/home/dev/workshop/src/screen/file_viewer.rs");
        let segs = breadcrumb_segments(&root, &file);
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].label, "workshop");
        assert_eq!(segs[0].path, PathBuf::from("/home/dev/workshop"));
        assert!(!segs[0].is_file);
        assert_eq!(segs[1].label, "src");
        assert_eq!(segs[1].path, PathBuf::from("/home/dev/workshop/src"));
        assert!(!segs[1].is_file);
        assert_eq!(segs[2].label, "screen");
        assert_eq!(
            segs[2].path,
            PathBuf::from("/home/dev/workshop/src/screen")
        );
        assert!(!segs[2].is_file);
        assert_eq!(segs[3].label, "file_viewer.rs");
        assert_eq!(
            segs[3].path,
            PathBuf::from("/home/dev/workshop/src/screen/file_viewer.rs")
        );
        assert!(segs[3].is_file);
    }

    #[test]
    fn breadcrumb_segments_root_only_when_outside_workshop() {
        let root = PathBuf::from("/home/dev/workshop");
        let file = PathBuf::from("/tmp/loose.txt");
        let segs = breadcrumb_segments(&root, &file);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].label, "workshop");
        assert!(!segs[0].is_file);
    }

    #[test]
    fn breadcrumb_segments_file_at_workshop_root() {
        let root = PathBuf::from("/home/dev/workshop");
        let file = PathBuf::from("/home/dev/workshop/README.md");
        let segs = breadcrumb_segments(&root, &file);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].label, "workshop");
        assert!(!segs[0].is_file);
        assert_eq!(segs[1].label, "README.md");
        assert!(segs[1].is_file);
    }

    #[test]
    fn total_lines_reflects_loaded_content() {
        let mut state = FileViewerState::new(PathBuf::from("foo.rs"));
        state.lines = vec![
            HighlightedLine { spans: Vec::new() },
            HighlightedLine { spans: Vec::new() },
            HighlightedLine { spans: Vec::new() },
        ];
        assert_eq!(state.total_lines(), 3);
    }
}
