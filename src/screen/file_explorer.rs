// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! File explorer panel — displays project tree with git/worktree awareness.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use data::git::{DiffStat, FileStatus};
use iced::widget::{Space, button, column, container, row, scrollable, svg, text};
use iced::{Color, Element, Length, Theme};

use crate::icons;
use crate::style::{self, FONT_BODY, FONT_HEADER, FONT_ICON, FONT_ICON_SM, FONT_LABEL, Palette};

// ── Messages ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    SelectFile(PathBuf),
    ToggleDirectory(PathBuf),
    Refresh,
    TreeLoaded(
        Vec<FileNode>,
        HashMap<PathBuf, FileStatus>,
        HashMap<PathBuf, DiffStat>,
        Option<String>,
    ),
    LinkSpark(PathBuf),
}

// ── File tree types ───────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileNode {
    pub path: PathBuf,
    pub name: String,
    pub kind: NodeKind,
    pub children: Vec<FileNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Directory,
}

// ── State ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileExplorerState {
    /// Root nodes of the file tree.
    pub tree: Vec<FileNode>,
    /// Directories currently expanded.
    pub expanded: HashSet<PathBuf>,
    /// Git status per file (repo-relative paths).
    pub git_statuses: HashMap<PathBuf, FileStatus>,
    /// Per-file diff stats (additions/deletions) keyed by repo-relative path.
    pub diff_stats: HashMap<PathBuf, DiffStat>,
    /// Current git branch name.
    pub branch: Option<String>,
    /// Currently selected file path.
    pub selected: Option<PathBuf>,
    /// Precomputed git status map covering files (direct lookup) and
    /// directories (aggregated). Rebuilt on every `FilesScanned` event.
    /// Spark ryve-252c5b6e.
    pub precomputed_git_statuses: HashMap<PathBuf, FileStatus>,
    /// Precomputed diff stat map covering files (direct lookup) and
    /// directories (aggregated). Rebuilt on every `FilesScanned` event.
    /// Spark ryve-252c5b6e.
    pub precomputed_diff_stats: HashMap<PathBuf, DiffStat>,
}

impl FileExplorerState {
    pub fn new() -> Self {
        Self {
            tree: Vec::new(),
            expanded: HashSet::new(),
            git_statuses: HashMap::new(),
            diff_stats: HashMap::new(),
            branch: None,
            selected: None,
            precomputed_git_statuses: HashMap::new(),
            precomputed_diff_stats: HashMap::new(),
        }
    }

    /// Rebuild the precomputed status/diff maps from the raw git data.
    /// Call after `git_statuses` or `diff_stats` are updated.
    /// Spark ryve-252c5b6e.
    pub fn rebuild_precomputed_maps(&mut self) {
        self.precomputed_git_statuses = perf_core::precompute_git_status_map(&self.git_statuses);
        self.precomputed_diff_stats = perf_core::precompute_diff_stat_map(&self.diff_stats);
    }
}

// ── Async scanning ────────────────────────────────────

/// Scan a directory and build a file tree + git statuses.
/// Call this via `Task::perform`.
pub async fn scan_directory(
    root: PathBuf,
    ignore: Vec<String>,
) -> (
    Vec<FileNode>,
    HashMap<PathBuf, FileStatus>,
    HashMap<PathBuf, DiffStat>,
    Option<String>,
) {
    let ignore = std::sync::Arc::new(ignore);
    let tree = build_tree(root.clone(), 0, ignore).await;

    let repo = data::git::Repository::new(&root);
    let is_repo = data::git::Repository::is_repo(&root).await;

    let (statuses, diff_stats, branch) = if is_repo {
        let statuses = repo.file_statuses().await.unwrap_or_default();
        let diff_stats = repo.diff_stats().await.unwrap_or_default();
        let branch = repo.current_branch().await.ok();
        (statuses, diff_stats, branch)
    } else {
        (HashMap::new(), HashMap::new(), None)
    };

    (tree, statuses, diff_stats, branch)
}

/// Collected entry info for sorting before recursion.
struct RawEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

/// Recursively build the file tree, skipping ignored entries.
fn build_tree(
    path: PathBuf,
    depth: u32,
    ignore: std::sync::Arc<Vec<String>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<FileNode>> + Send>> {
    Box::pin(async move {
        let mut nodes = Vec::new();

        if depth > 12 {
            return nodes;
        }

        let mut entries = match tokio::fs::read_dir(&path).await {
            Ok(rd) => rd,
            Err(_) => return nodes,
        };

        // Collect entries with their metadata
        let mut raw: Vec<RawEntry> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".#") || ignore.iter().any(|p| p == &name) {
                continue;
            }
            let is_dir = entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false);
            raw.push(RawEntry {
                path: entry.path(),
                name,
                is_dir,
            });
        }

        // Sort: directories first, then alphabetical (case-insensitive)
        raw.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a
                .name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase()),
        });

        for entry in raw {
            if entry.is_dir {
                let children = build_tree(entry.path.clone(), depth + 1, ignore.clone()).await;
                nodes.push(FileNode {
                    path: entry.path,
                    name: entry.name,
                    kind: NodeKind::Directory,
                    children,
                });
            } else {
                nodes.push(FileNode {
                    path: entry.path,
                    name: entry.name,
                    kind: NodeKind::File,
                    children: Vec::new(),
                });
            }
        }

        nodes
    })
}

// ── View ──────────────────────────────────────────────

/// Render the file explorer panel.
pub fn view<'a>(
    state: &'a FileExplorerState,
    root: &'a Path,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;
    let branch_label = state.branch.as_deref().unwrap_or("no branch");

    let root_icon = svg(icons::root_folder_icon(true)).width(16).height(16);

    let header = row![
        root_icon,
        text("Files").size(FONT_HEADER).color(pal.text_primary),
        Space::new().width(Length::Fill),
        text(branch_label)
            .size(FONT_LABEL)
            .color(pal.text_secondary),
        button(text("\u{21BB}").size(FONT_ICON))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::Refresh),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center)
    .padding([8, 10]);

    let mut items: Vec<Element<'a, Message>> = Vec::new();

    if state.tree.is_empty() {
        items.push(
            container(text("Scanning...").size(FONT_BODY))
                .padding([8, 10])
                .into(),
        );
    } else {
        for node in &state.tree {
            collect_nodes(node, root, state, 0, &pal, &mut items);
        }
    }

    let list = iced::widget::Column::with_children(items)
        .spacing(0)
        .padding([0, 10]);

    let content = column![
        header,
        scrollable(list)
            .height(Length::Fill)
            .style(scrollable::default)
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    content.into()
}

fn collect_nodes<'a>(
    node: &'a FileNode,
    root: &'a Path,
    state: &'a FileExplorerState,
    depth: u16,
    pal: &Palette,
    items: &mut Vec<Element<'a, Message>>,
) {
    let indent = (depth as f32) * 16.0;
    let is_expanded = state.expanded.contains(&node.path);
    let is_selected = state.selected.as_ref() == Some(&node.path);

    // Look up git status and diff stats from precomputed maps (O(1) per
    // node instead of O(files) for directories). Spark ryve-252c5b6e.
    let rel_path = node.path.strip_prefix(root).unwrap_or(&node.path);
    let git_status = state.precomputed_git_statuses.get(rel_path).copied();
    let git_color = match git_status {
        Some(status) => status_color(status),
        None => pal.text_primary,
    };

    let icon_handle = match node.kind {
        NodeKind::Directory => icons::folder_icon(&node.name, is_expanded),
        NodeKind::File => icons::file_icon(&node.name),
    };
    let icon_widget = svg(icon_handle).width(16).height(16);

    let diff = state
        .precomputed_diff_stats
        .get(rel_path)
        .copied()
        .unwrap_or_default();

    let mut label = row![
        Space::new().width(indent),
        icon_widget,
        text(&node.name).size(FONT_BODY).color(git_color),
        Space::new().width(Length::Fill),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center);

    // Accessible text badge for git status (colorblind-friendly alongside the color).
    if let Some(status) = git_status {
        label = label.push(
            text(status_letter(status).to_string())
                .size(FONT_LABEL)
                .font(iced::Font::MONOSPACE)
                .color(status_color(status)),
        );
    }

    // Show +N / -N badges when there are changes
    if diff.additions > 0 {
        label = label.push(
            text(format!("+{}", diff.additions))
                .size(FONT_LABEL)
                .color(Color::from_rgb(0.40, 0.85, 0.45)),
        );
    }
    if diff.deletions > 0 {
        label = label.push(
            text(format!("-{}", diff.deletions))
                .size(FONT_LABEL)
                .color(Color::from_rgb(0.90, 0.35, 0.35)),
        );
    }

    // Spark link button
    label = label.push(
        button(text("\u{26A1}").size(FONT_ICON_SM))
            .style(button::text)
            .padding([1, 3])
            .on_press(Message::LinkSpark(node.path.clone())),
    );

    let msg = match node.kind {
        NodeKind::Directory => Message::ToggleDirectory(node.path.clone()),
        NodeKind::File => Message::SelectFile(node.path.clone()),
    };

    if is_selected {
        let pal_copy = *pal;
        let item = container(
            button(label)
                .style(button::text)
                .width(Length::Fill)
                .padding([2, 4])
                .on_press(msg),
        )
        .style(move |_theme: &Theme| style::selected_item(&pal_copy));
        items.push(item.into());
    } else {
        let btn = button(label)
            .style(style::row_button(*pal))
            .width(Length::Fill)
            .padding([2, 4])
            .on_press(msg);
        items.push(btn.into());
    }

    // Render children if directory is expanded
    if node.kind == NodeKind::Directory && is_expanded {
        for child in &node.children {
            collect_nodes(child, root, state, depth + 1, pal, items);
        }
    }
}

// ── Git status colors ─────────────────────────────────

/// Convert the local file-explorer node kind into the perf_core variant
/// the shared aggregation functions expect. Only used in tests now that
/// the view uses precomputed maps. Spark ryve-252c5b6e.
#[cfg(test)]
fn perf_node_kind(kind: &NodeKind) -> perf_core::NodeKind {
    match kind {
        NodeKind::File => perf_core::NodeKind::File,
        NodeKind::Directory => perf_core::NodeKind::Directory,
    }
}

/// Resolve the effective git status for a file or directory.
///
/// Implementation lives in `perf_core` so the regression harness benches
/// the same code path the UI hits on every redraw. Spark ryve-5b9c5d93.
/// Only used in tests now that the view uses precomputed maps.
#[cfg(test)]
fn file_git_status(
    rel_path: &Path,
    kind: &NodeKind,
    statuses: &HashMap<PathBuf, FileStatus>,
) -> Option<FileStatus> {
    perf_core::file_git_status(rel_path, perf_node_kind(kind), statuses)
}

/// Get aggregated diff stats for a file or directory.
///
/// Implementation lives in `perf_core` (see [`file_git_status`]).
/// Only used in tests now that the view uses precomputed maps.
#[cfg(test)]
fn file_diff_stat(
    rel_path: &Path,
    kind: &NodeKind,
    diff_stats: &HashMap<PathBuf, DiffStat>,
) -> DiffStat {
    perf_core::file_diff_stat(rel_path, perf_node_kind(kind), diff_stats)
}

fn status_color(status: FileStatus) -> Color {
    match status {
        FileStatus::Modified => Color::from_rgb(0.90, 0.80, 0.30), // yellow
        FileStatus::Added => Color::from_rgb(0.40, 0.85, 0.45),    // green
        FileStatus::Deleted => Color::from_rgb(0.90, 0.35, 0.35),  // red
        FileStatus::Renamed => Color::from_rgb(0.55, 0.75, 0.95),  // blue
        FileStatus::Copied => Color::from_rgb(0.55, 0.75, 0.95),   // blue
        FileStatus::Untracked => Color::from_rgb(0.55, 0.55, 0.55), // dim grey
        FileStatus::Ignored => Color::from_rgb(0.40, 0.40, 0.40),  // darker grey
        FileStatus::Conflicted => Color::from_rgb(1.0, 0.45, 0.20), // orange
    }
}

/// Short accessible letter label for a git status, rendered alongside the
/// status color so colorblind users can still distinguish states.
fn status_letter(status: FileStatus) -> char {
    match status {
        FileStatus::Modified => 'M',
        FileStatus::Added => 'A',
        FileStatus::Deleted => 'D',
        FileStatus::Renamed => 'R',
        FileStatus::Copied => 'C',
        FileStatus::Untracked => '?',
        FileStatus::Ignored => 'I',
        FileStatus::Conflicted => 'U',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_letter_covers_all_variants() {
        assert_eq!(status_letter(FileStatus::Modified), 'M');
        assert_eq!(status_letter(FileStatus::Added), 'A');
        assert_eq!(status_letter(FileStatus::Deleted), 'D');
        assert_eq!(status_letter(FileStatus::Renamed), 'R');
        assert_eq!(status_letter(FileStatus::Copied), 'C');
        assert_eq!(status_letter(FileStatus::Untracked), '?');
        assert_eq!(status_letter(FileStatus::Conflicted), 'U');
        assert_eq!(status_letter(FileStatus::Ignored), 'I');
    }

    #[test]
    fn file_git_status_returns_direct_file_status() {
        let mut statuses = HashMap::new();
        statuses.insert(PathBuf::from("src/main.rs"), FileStatus::Modified);

        let got = file_git_status(Path::new("src/main.rs"), &NodeKind::File, &statuses);
        assert_eq!(got, Some(FileStatus::Modified));

        let none = file_git_status(Path::new("src/other.rs"), &NodeKind::File, &statuses);
        assert_eq!(none, None);
    }

    #[test]
    fn file_git_status_aggregates_directory_children() {
        let mut statuses = HashMap::new();
        statuses.insert(PathBuf::from("src/a.rs"), FileStatus::Modified);
        statuses.insert(PathBuf::from("src/b.rs"), FileStatus::Conflicted);
        statuses.insert(PathBuf::from("src/c.rs"), FileStatus::Untracked);

        // Directory should take the highest-priority child status (Conflicted).
        let got = file_git_status(Path::new("src"), &NodeKind::Directory, &statuses);
        assert_eq!(got, Some(FileStatus::Conflicted));
    }

    #[test]
    fn file_git_status_empty_directory_is_none() {
        let statuses: HashMap<PathBuf, FileStatus> = HashMap::new();
        let got = file_git_status(Path::new("src"), &NodeKind::Directory, &statuses);
        assert_eq!(got, None);
    }

    #[test]
    fn file_git_status_directory_does_not_match_sibling_with_same_prefix() {
        // Regression for the PR #5 Copilot review (spark ryve-20e0fa52):
        // a string-prefix check would have made directory `src` light up
        // because of changes inside `src2/`. With `Path::starts_with` the
        // boundary is component-aware and `src` must report `None`.
        let mut statuses = HashMap::new();
        statuses.insert(PathBuf::from("src2/foo.rs"), FileStatus::Modified);
        statuses.insert(PathBuf::from("src22/bar.rs"), FileStatus::Modified);

        let got = file_git_status(Path::new("src"), &NodeKind::Directory, &statuses);
        assert_eq!(
            got, None,
            "directory `src` must not absorb siblings `src2/`"
        );

        // The actual `src2` directory should still report Modified.
        let got = file_git_status(Path::new("src2"), &NodeKind::Directory, &statuses);
        assert_eq!(got, Some(FileStatus::Modified));
    }

    #[test]
    fn file_diff_stat_directory_does_not_match_sibling_with_same_prefix() {
        // Same regression, applied to the diff_stat aggregation. Spark
        // ryve-20e0fa52: `file_diff_stat` previously used
        // `to_string_lossy().starts_with()` and would have folded changes
        // from `src2/foo.rs` into directory `src`'s totals.
        let mut diff_stats = HashMap::new();
        diff_stats.insert(
            PathBuf::from("src2/foo.rs"),
            DiffStat {
                additions: 7,
                deletions: 3,
            },
        );
        diff_stats.insert(
            PathBuf::from("src/lib.rs"),
            DiffStat {
                additions: 2,
                deletions: 1,
            },
        );

        // `src` must only see its own descendants.
        let got = file_diff_stat(Path::new("src"), &NodeKind::Directory, &diff_stats);
        assert_eq!(got.additions, 2);
        assert_eq!(got.deletions, 1);

        // `src2` must only see its own descendants.
        let got = file_diff_stat(Path::new("src2"), &NodeKind::Directory, &diff_stats);
        assert_eq!(got.additions, 7);
        assert_eq!(got.deletions, 3);
    }
}
