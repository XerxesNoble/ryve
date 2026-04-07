// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Workgraph panel — displays and manages sparks for the active workshop.

use data::sparks::types::Spark;
use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Theme};

use crate::style::{
    self, FONT_BODY, FONT_HEADER, FONT_ICON, FONT_ICON_SM, FONT_LABEL, FONT_SMALL, Palette,
};

// ── State ────────────────────────────────────────────

/// Inline create form state, held on the Workshop. The form enforces a
/// minimum set of fields before submission: title, type, priority, problem
/// statement, at least one acceptance criterion, and (when the type is not
/// `epic`) a parent epic to nest the new spark under.
#[derive(Debug, Clone, Default)]
pub struct CreateForm {
    pub title: String,
    pub spark_type: String,
    pub priority: i32,
    pub problem: String,
    pub acceptance: String,
    pub parent_epic_id: Option<String>,
    pub error: Option<String>,
    pub visible: bool,
}

impl CreateForm {
    /// Reset to a clean form ready for the next "+" click. Defaults to
    /// `task` / P2 / no parent so the user only has to fill in the
    /// remaining mandatory fields.
    pub fn reset(&mut self) {
        self.title.clear();
        self.spark_type = "task".to_string();
        self.priority = 2;
        self.problem.clear();
        self.acceptance.clear();
        self.parent_epic_id = None;
        self.error = None;
    }

    /// Validate the form and return the first missing-field error, if
    /// any. `Ok(())` means the form is safe to submit.
    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("Title is required.".to_string());
        }
        if self.spark_type.is_empty() {
            return Err("Pick a spark type.".to_string());
        }
        if self.problem.trim().is_empty() {
            return Err("Problem statement is required.".to_string());
        }
        if self.acceptance.trim().is_empty() {
            return Err("At least one acceptance criterion is required.".to_string());
        }
        if self.spark_type != "epic" && self.parent_epic_id.is_none() {
            return Err("Pick a parent epic (only epics may be top-level).".to_string());
        }
        Ok(())
    }
}

// ── Messages ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    SelectSpark(String),
    Refresh,
    ShowCreateForm,
    CreateFormTitleChanged(String),
    CreateFormTypeChanged(String),
    CreateFormPriorityChanged(i32),
    CreateFormProblemChanged(String),
    CreateFormAcceptanceChanged(String),
    CreateFormParentEpicChanged(Option<String>),
    SubmitNewSpark,
    CancelCreate,
    /// Quick status cycle: open → in_progress → closed
    CycleStatus(String, String),
}

// ── View ─────────────────────────────────────────────

pub fn view<'a>(
    sparks: &'a [Spark],
    pal: &Palette,
    has_bg: bool,
    create_form: &'a CreateForm,
) -> Element<'a, Message> {
    let pal = *pal;

    let header = row![
        text("Workgraph").size(FONT_HEADER).color(pal.text_primary),
        Space::new().width(Length::Fill),
        button(text("+").size(FONT_ICON).color(pal.accent))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::ShowCreateForm),
        button(text("\u{21BB}").size(FONT_ICON).color(pal.text_secondary))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::Refresh),
    ]
    .spacing(4)
    .padding([8, 10]);

    let mut list = column![].spacing(2).padding([0, 10]);

    // Inline create form
    if create_form.visible {
        list = list.push(view_create_form(sparks, create_form, &pal));
    }

    if sparks.is_empty() && !create_form.visible {
        list = list.push(
            text("No sparks yet")
                .size(FONT_BODY)
                .color(pal.text_tertiary),
        );
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

// ── Create form view ─────────────────────────────────

const SPARK_TYPES: &[(&str, &str)] = &[
    ("task", "Task"),
    ("bug", "Bug"),
    ("feature", "Feature"),
    ("chore", "Chore"),
    ("spike", "Spike"),
    ("milestone", "Milestone"),
    ("epic", "Epic"),
];

fn view_create_form<'a>(
    sparks: &'a [Spark],
    form: &'a CreateForm,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    // ── type chips ──
    let mut type_chips = row![].spacing(4).align_y(iced::Alignment::Center);
    for (key, label) in SPARK_TYPES {
        let selected = form.spark_type == *key;
        let key_owned = (*key).to_string();
        type_chips = type_chips.push(form_chip(label, selected, &pal, move || {
            Message::CreateFormTypeChanged(key_owned.clone())
        }));
    }

    // ── priority chips ──
    let mut prio_chips = row![].spacing(4).align_y(iced::Alignment::Center);
    for p in 0..=4i32 {
        let selected = form.priority == p;
        prio_chips = prio_chips.push(form_chip(
            &format!("P{p}"),
            selected,
            &pal,
            move || Message::CreateFormPriorityChanged(p),
        ));
    }

    // ── parent epic chips ──
    // Only relevant when type != epic. Lists every epic spark in the
    // workshop so the user can attach the new spark to one.
    let parent_section: Element<Message> = if form.spark_type == "epic" {
        text("Epics are top-level (no parent required).")
            .size(FONT_SMALL)
            .color(pal.text_tertiary)
            .into()
    } else {
        let mut chips = row![].spacing(4).align_y(iced::Alignment::Center);
        let epics: Vec<&Spark> = sparks.iter().filter(|s| s.spark_type == "epic").collect();
        if epics.is_empty() {
            chips = chips.push(
                text("No epics exist yet — create one first.")
                    .size(FONT_SMALL)
                    .color(pal.text_tertiary),
            );
        } else {
            for epic in epics {
                let epic_id = epic.id.clone();
                let selected = form.parent_epic_id.as_deref() == Some(epic.id.as_str());
                chips = chips.push(form_chip(&epic.title, selected, &pal, move || {
                    Message::CreateFormParentEpicChanged(Some(epic_id.clone()))
                }));
            }
        }
        scrollable(chips)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new(),
            ))
            .into()
    };

    // ── inputs ──
    let title_input = text_input("Title (required)", &form.title)
        .size(FONT_BODY)
        .padding([6, 8])
        .on_input(Message::CreateFormTitleChanged)
        .on_submit(Message::SubmitNewSpark);

    let problem_input = text_input("Problem statement (required)", &form.problem)
        .size(FONT_BODY)
        .padding([6, 8])
        .on_input(Message::CreateFormProblemChanged);

    let acceptance_input = text_input("Acceptance criterion (required)", &form.acceptance)
        .size(FONT_BODY)
        .padding([6, 8])
        .on_input(Message::CreateFormAcceptanceChanged);

    // ── error banner ──
    let error_banner: Element<Message> = if let Some(err) = &form.error {
        text(err.as_str()).size(FONT_SMALL).color(pal.danger).into()
    } else {
        Space::new().height(0).into()
    };

    let actions = row![
        button(text("Create").size(FONT_LABEL).color(pal.accent))
            .style(button::text)
            .padding([3, 8])
            .on_press(Message::SubmitNewSpark),
        button(text("Cancel").size(FONT_LABEL).color(pal.text_tertiary))
            .style(button::text)
            .padding([3, 8])
            .on_press(Message::CancelCreate),
    ]
    .spacing(8);

    column![
        section_label("Title", &pal),
        title_input,
        section_label("Type", &pal),
        type_chips,
        section_label("Priority", &pal),
        prio_chips,
        section_label("Parent epic", &pal),
        parent_section,
        section_label("Problem statement", &pal),
        problem_input,
        section_label("Acceptance criterion", &pal),
        acceptance_input,
        error_banner,
        actions,
    ]
    .spacing(6)
    .padding([6, 0])
    .into()
}

fn section_label<'a>(label: &'a str, pal: &Palette) -> Element<'a, Message> {
    text(label)
        .size(FONT_LABEL)
        .color(pal.text_tertiary)
        .into()
}

fn form_chip<'a, F>(
    label: &str,
    selected: bool,
    pal: &Palette,
    on_press: F,
) -> Element<'a, Message>
where
    F: 'a + Fn() -> Message,
{
    let pal = *pal;
    let text_color = if selected {
        pal.window_bg
    } else {
        pal.text_primary
    };
    button(text(label.to_string()).size(FONT_LABEL).color(text_color))
        .style(move |_t: &Theme, _s| button::Style {
            background: Some(iced::Background::Color(if selected {
                pal.accent
            } else {
                pal.surface
            })),
            text_color,
            border: iced::Border {
                color: pal.border,
                width: 1.0,
                radius: iced::border::Radius::from(8.0),
            },
            ..button::Style::default()
        })
        .padding([3, 8])
        .on_press_with(on_press)
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

    let next_status = next_status_str(&spark.status);
    let priority_label = format!("P{}", spark.priority);
    let id = spark.id.clone();

    let status_btn = button(
        text(status_indicator)
            .size(FONT_ICON_SM)
            .color(pal.text_secondary),
    )
    .style(button::text)
    .padding([2, 4])
    .on_press(Message::CycleStatus(id.clone(), next_status.to_string()));

    row![
        status_btn,
        button(
            row![
                text(priority_label)
                    .size(FONT_LABEL)
                    .color(pal.text_tertiary),
                text(&spark.title).size(FONT_BODY).color(pal.text_primary),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .style(button::text)
        .width(Length::Fill)
        .padding([5, 6])
        .on_press(Message::SelectSpark(id))
    ]
    .spacing(2)
    .align_y(iced::Alignment::Center)
    .into()
}

/// Cycle: open → in_progress → closed → open
fn next_status_str(current: &str) -> &'static str {
    match current {
        "open" => "in_progress",
        "in_progress" => "closed",
        "closed" => "open",
        "blocked" => "open",
        "deferred" => "open",
        _ => "open",
    }
}
