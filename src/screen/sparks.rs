// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Workgraph panel — displays the issue tracker for the active workshop.

use data::sparks::types::Spark;
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Element, Length, Theme};

use crate::style::{self, Palette};

#[derive(Debug, Clone)]
pub enum Message {
    SelectSpark(String),
    Refresh,
}

/// Render the sparks panel given a list of sparks.
pub fn view<'a>(sparks: &'a [Spark], pal: &Palette, has_bg: bool) -> Element<'a, Message> {
    let pal = *pal;

    let header = row![
        text("Workgraph").size(14).color(pal.text_primary),
        Space::new().width(Length::Fill),
        button(text("\u{21BB}").size(13).color(pal.text_secondary))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::Refresh),
    ]
    .padding([8, 10]);

    let mut list = column![].spacing(2).padding([0, 10]);

    if sparks.is_empty() {
        list = list.push(text("No sparks yet").size(12).color(pal.text_tertiary));
    } else {
        for spark in sparks {
            list = list.push(view_spark_row(spark, &pal));
        }
    }

    let content = column![header, scrollable(list).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_theme: &Theme| style::glass_panel(&pal, has_bg))
        .into()
}

fn view_spark_row<'a>(spark: &'a Spark, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let status_indicator: &str = match spark.status.as_str() {
        "open" => "\u{25CB}",        // ○
        "in_progress" => "\u{25D4}", // ◔
        "blocked" => "\u{25A0}",     // ■
        "deferred" => "\u{25CC}",    // ◌
        "closed" => "\u{25CF}",      // ●
        _ => "\u{25CB}",
    };

    let priority_label: String = format!("P{}", spark.priority);
    let id = spark.id.clone();

    button(
        row![
            text(status_indicator).size(12).color(pal.text_secondary),
            text(priority_label).size(10).color(pal.text_tertiary),
            text(&spark.title).size(12).color(pal.text_primary),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    )
    .style(button::text)
    .width(Length::Fill)
    .padding([4, 6])
    .on_press(Message::SelectSpark(id))
    .into()
}
