// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Sparks panel — displays the issue tracker for the active workshop.

use data::sparks::types::Spark;
use iced::widget::{button, column, container, row, scrollable, text, Space};
use iced::{Element, Length};

#[derive(Debug, Clone)]
pub enum Message {
    SelectSpark(String),
    Refresh,
}

/// Render the sparks panel given a list of sparks.
pub fn view<'a>(sparks: &'a [Spark]) -> Element<'a, Message> {
    let header = row![
        text("Sparks").size(14),
        Space::new().width(Length::Fill),
        button(text("\u{21BB}").size(13))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::Refresh),
    ]
    .padding([8, 10]);

    let mut list = column![].spacing(2).padding([0, 10]);

    if sparks.is_empty() {
        list = list.push(text("No sparks yet").size(12));
    } else {
        for spark in sparks {
            list = list.push(view_spark_row(spark));
        }
    }

    let content = column![header, scrollable(list).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(container::bordered_box)
        .into()
}

fn view_spark_row(spark: &Spark) -> Element<'_, Message> {
    let status_indicator: &str = match spark.status.as_str() {
        "open" => "\u{25CB}",           // ○
        "in_progress" => "\u{25D4}",    // ◔
        "blocked" => "\u{25A0}",        // ■
        "deferred" => "\u{25CC}",       // ◌
        "closed" => "\u{25CF}",         // ●
        _ => "\u{25CB}",
    };

    let priority_label: String = format!("P{}", spark.priority);
    let id = spark.id.clone();

    button(
        row![
            text(status_indicator).size(12),
            text(priority_label).size(10),
            text(&spark.title).size(12),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    )
    .style(button::text)
    .width(Length::Fill)
    .padding([4, 4])
    .on_press(Message::SelectSpark(id))
    .into()
}
