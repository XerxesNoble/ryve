// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Status bar — rich bottom bar showing git branch, diff stats, spark breakdown, agents, and settings.

use iced::widget::{Space, button, container, row, text};
use iced::{Element, Length, Theme};

use crate::style::{self, Palette, FONT_ICON, FONT_LABEL};

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

/// Render the status bar for a workshop.
pub fn view<'a>(
    branch: Option<&'a str>,
    directory: &'a std::path::Path,
    spark_summary: &SparkSummary,
    git_stats: &GitStats,
    active_agents: usize,
    total_agents: usize,
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

    // Git branch — clickable to switch
    if let Some(branch) = branch {
        let branch_btn = button(
            row![
                text("\u{E0A0}").size(FONT_LABEL).color(pal.accent),
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

    // ── Right section: agents + settings ─────────────────
    let mut right = row![].spacing(14).align_y(iced::Alignment::Center);

    // Active agents indicator
    if total_agents > 0 {
        let agent_color = if active_agents > 0 {
            green
        } else {
            pal.text_tertiary
        };

        let agent_label = if active_agents > 0 {
            format!(
                "{} agent{} running",
                active_agents,
                if active_agents == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "{} agent{}",
                total_agents,
                if total_agents == 1 { "" } else { "s" }
            )
        };

        right = right.push(
            row![
                text("●").size(FONT_LABEL).color(agent_color),
                text(agent_label).size(FONT_LABEL).color(pal.text_secondary),
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
