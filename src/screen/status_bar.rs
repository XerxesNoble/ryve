// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Status bar — rich bottom bar showing git branch, diff stats, spark breakdown,
//! file viewer position/language, active Hands, failing contracts, and settings.

use data::sparks::types::ATLAS_NAME;
use iced::widget::{Space, button, container, row, text};
use iced::{Element, Length, Theme};

use crate::style::{self, FONT_ICON, FONT_LABEL, Palette};

/// Role annotation rendered next to [`ATLAS_NAME`] in the status bar.
pub const ATLAS_ROLE_ANNOTATION: &str = "(Director)";

#[derive(Debug, Clone)]
pub enum Message {
    OpenSettings,
    RequestBranchSwitch,
}

/// Summary of spark statuses for the status bar.
#[derive(Debug, Clone, Default)]
pub struct SparkSummary {
    pub open: usize,
    pub in_progress: usize,
    pub blocked: usize,
    pub deferred: usize,
    pub closed: usize,
}

impl SparkSummary {
    pub fn total_active(&self) -> usize {
        self.open + self.in_progress + self.blocked + self.deferred
    }
}

/// Aggregated git diff statistics.
#[derive(Debug, Clone, Default)]
pub struct GitStats {
    pub changed_files: usize,
    pub additions: u32,
    pub deletions: u32,
}

/// Information about the currently focused file viewer, if any.
#[derive(Debug, Clone)]
pub struct FileViewerInfo<'a> {
    /// 1-indexed cursor / selection line.
    pub line: usize,
    /// 1-indexed cursor / selection column.
    pub column: usize,
    /// Total number of lines in the file (0 if unknown).
    pub total_lines: usize,
    /// Display label for the language mode (e.g. "Rust", "Markdown").
    pub language: &'a str,
}

/// Render the status bar for a workshop.
#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    branch: Option<&'a str>,
    directory: &'a std::path::Path,
    spark_summary: &SparkSummary,
    git_stats: &GitStats,
    active_hands: usize,
    total_hands: usize,
    failing_contracts: usize,
    file_info: Option<FileViewerInfo<'a>>,
    pal: &Palette,
    has_bg: bool,
) -> Element<'a, Message> {
    let pal = *pal;

    // Colors for diff display
    let green = iced::Color {
        r: 0.298,
        g: 0.851,
        b: 0.392,
        a: 1.0,
    };
    let red = iced::Color {
        r: 1.0,
        g: 0.388,
        b: 0.353,
        a: 1.0,
    };

    // ── Left section: git branch + directory + diffs ─────
    let mut left = row![].spacing(14).align_y(iced::Alignment::Center);

    // Git branch — clickable to switch.
    //
    // Use a font-agnostic glyph (`⎇`, U+2387 ALTERNATIVE KEY SYMBOL) instead
    // of the Powerline branch glyph (U+E0A0) which lives in the Private Use
    // Area and only renders with a Nerd Font installed. The previous icon
    // showed as a tofu box on default fonts.
    if let Some(branch) = branch {
        let branch_btn = button(
            row![
                text("\u{2387}").size(FONT_LABEL).color(pal.accent),
                text(branch).size(FONT_LABEL).color(pal.text_primary),
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center),
        )
        .style(button::text)
        .padding([2, 6])
        .on_press(Message::RequestBranchSwitch);

        left = left.push(branch_btn);
    }

    // Git diff stats
    if git_stats.changed_files > 0 {
        left = left.push(separator(&pal));

        // Changed file count
        left = left.push(
            text(format!(
                "{} file{}",
                git_stats.changed_files,
                if git_stats.changed_files == 1 {
                    ""
                } else {
                    "s"
                },
            ))
            .size(12)
            .color(pal.text_secondary),
        );

        // +additions / -deletions
        let mut diff_row = row![].spacing(6).align_y(iced::Alignment::Center);
        if git_stats.additions > 0 {
            diff_row = diff_row.push(
                text(format!("+{}", git_stats.additions))
                    .size(12)
                    .color(green),
            );
        }
        if git_stats.deletions > 0 {
            diff_row = diff_row.push(
                text(format!("\u{2212}{}", git_stats.deletions))
                    .size(12)
                    .color(red),
            );
        }
        left = left.push(diff_row);
    }

    left = left.push(separator(&pal));

    // Working directory
    let dir_name = directory
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workshop");
    left = left.push(text(dir_name).size(FONT_LABEL).color(pal.text_secondary));

    // ── Center section: spark breakdown ──────────────────
    let mut center = row![].spacing(12).align_y(iced::Alignment::Center);

    let total_active = spark_summary.total_active();
    if total_active > 0 || spark_summary.closed > 0 {
        // Spark icon
        center = center.push(
            text("\u{2726}") // ✦
                .size(FONT_LABEL)
                .color(pal.text_tertiary),
        );

        // Status pills with counts
        if spark_summary.open > 0 {
            center = center.push(spark_pill("○", spark_summary.open, pal.text_secondary));
        }
        if spark_summary.in_progress > 0 {
            center = center.push(spark_pill("◔", spark_summary.in_progress, pal.accent));
        }
        if spark_summary.blocked > 0 {
            center = center.push(spark_pill("■", spark_summary.blocked, pal.danger));
        }
        if spark_summary.deferred > 0 {
            center = center.push(spark_pill("◌", spark_summary.deferred, pal.text_tertiary));
        }

        // Total active count
        center = center.push(
            text(format!("{} active", total_active))
                .size(FONT_LABEL)
                .color(pal.text_tertiary),
        );
    }

    // Failing contracts indicator (only shown when > 0).
    if failing_contracts > 0 {
        if total_active > 0 || spark_summary.closed > 0 {
            center = center.push(separator(&pal));
        }
        center = center.push(
            row![
                text("\u{26A0}").size(FONT_LABEL).color(pal.danger), // ⚠
                text(format!(
                    "{} failing contract{}",
                    failing_contracts,
                    if failing_contracts == 1 { "" } else { "s" }
                ))
                .size(FONT_LABEL)
                .color(pal.danger),
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center),
        );
    }

    // ── Right section: file info + hands + settings ──────
    let mut right = row![].spacing(14).align_y(iced::Alignment::Center);

    // File viewer position + language mode (only when a file viewer is active).
    if let Some(info) = file_info {
        let position = if info.total_lines > 0 {
            format!(
                "Ln {}, Col {}  /  {} lines",
                info.line, info.column, info.total_lines
            )
        } else {
            format!("Ln {}, Col {}", info.line, info.column)
        };
        right = right.push(text(position).size(FONT_LABEL).color(pal.text_secondary));
        right = right.push(separator(&pal));
        right = right.push(
            text(info.language)
                .size(FONT_LABEL)
                .color(pal.text_secondary),
        );
        right = right.push(separator(&pal));
    }

    // Atlas (Director) indicator — anchors the agent hierarchy in the status
    // bar so users always see who is in charge, whether or not Hands are
    // currently running. Sits immediately before the Hand count.
    right = right.push(
        row![
            text(ATLAS_NAME).size(FONT_LABEL).color(pal.text_primary),
            text(ATLAS_ROLE_ANNOTATION)
                .size(FONT_LABEL)
                .color(pal.text_tertiary),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    );
    right = right.push(separator(&pal));

    // Active Hand count indicator
    if total_hands > 0 {
        let hand_color = if active_hands > 0 {
            green
        } else {
            pal.text_tertiary
        };

        let hand_label = if active_hands > 0 {
            format!(
                "{} Hand{} running",
                active_hands,
                if active_hands == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "{} Hand{}",
                total_hands,
                if total_hands == 1 { "" } else { "s" }
            )
        };

        right = right.push(
            row![
                text("●").size(FONT_LABEL).color(hand_color),
                text(hand_label).size(FONT_LABEL).color(pal.text_secondary),
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center),
        );

        right = right.push(separator(&pal));
    }

    // Settings gear button
    right = right.push(
        button(text("\u{2699}").size(FONT_ICON).color(pal.text_secondary))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::OpenSettings),
    );

    // ── Assemble the bar ─────────────────────────────────
    let bar = row![
        left,
        Space::new().width(Length::Fill),
        center,
        Space::new().width(Length::Fill),
        right,
    ]
    .padding([6, 14])
    .align_y(iced::Alignment::Center);

    container(bar)
        .width(Length::Fill)
        .style(move |_theme: &Theme| style::status_bar_style(&pal, has_bg))
        .into()
}

/// A compact spark status pill: icon + count.
fn spark_pill<'a>(icon: &'a str, count: usize, color: iced::Color) -> Element<'a, Message> {
    row![
        text(icon).size(FONT_LABEL).color(color),
        text(count.to_string()).size(FONT_LABEL).color(color),
    ]
    .spacing(3)
    .align_y(iced::Alignment::Center)
    .into()
}

fn separator<'a>(pal: &Palette) -> Element<'a, Message> {
    text("\u{2502}")
        .size(FONT_LABEL)
        .color(pal.separator)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spark ryve-7aa4dcd8: the status bar is the always-visible diagnostics
    /// surface and must consistently identify Atlas as the Director. The
    /// constants are referenced directly by `view`, so locking their values
    /// pins the on-screen text without having to introspect iced widgets.
    #[test]
    fn status_bar_identifies_atlas_as_director() {
        assert_eq!(ATLAS_NAME, "Atlas");
        assert_eq!(ATLAS_ROLE_ANNOTATION, "(Director)");
    }

    #[test]
    fn spark_summary_total_active_excludes_closed() {
        let s = SparkSummary {
            open: 2,
            in_progress: 1,
            blocked: 1,
            deferred: 3,
            closed: 9,
        };
        assert_eq!(s.total_active(), 7);
    }

    #[test]
    fn spark_summary_default_is_zero() {
        let s = SparkSummary::default();
        assert_eq!(s.total_active(), 0);
    }

    #[test]
    fn file_viewer_info_holds_position() {
        let info = FileViewerInfo {
            line: 12,
            column: 5,
            total_lines: 200,
            language: "Rust",
        };
        assert_eq!(info.line, 12);
        assert_eq!(info.column, 5);
        assert_eq!(info.total_lines, 200);
        assert_eq!(info.language, "Rust");
    }
}
