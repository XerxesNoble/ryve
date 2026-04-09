// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Workgraph panel — displays and manages sparks for the active workshop.

use std::collections::HashSet;

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

// ── Default sort ─────────────────────────────────────

/// Canonical ordering of `spark_type` values for the default sort. Any
/// value not in this list sorts after every listed type but stably
/// relative to other unknown types (via the string tiebreaker).
const SPARK_TYPE_ORDER: &[&str] = &[
    "epic",
    "bug",
    "feature",
    "task",
    "spike",
    "chore",
    "milestone",
];

/// Canonical ordering of `status` values for the default sort. Same
/// "unknown sinks to the end" rule as [`SPARK_TYPE_ORDER`].
const STATUS_ORDER: &[&str] = &[
    "in_progress",
    "blocked",
    "open",
    "deferred",
    "completed",
    "closed",
];

fn spark_type_rank(ty: &str) -> usize {
    SPARK_TYPE_ORDER
        .iter()
        .position(|t| *t == ty)
        .unwrap_or(SPARK_TYPE_ORDER.len())
}

fn status_rank(status: &str) -> usize {
    STATUS_ORDER
        .iter()
        .position(|s| *s == status)
        .unwrap_or(STATUS_ORDER.len())
}

/// Pure function: return `sparks` ordered by priority ASC, then by
/// `spark_type` (fixed order), then `status` (fixed order), then `id`
/// ASC as a deterministic tiebreaker. The input slice is not mutated.
///
/// Two consecutive calls with identical input produce identical output.
pub fn default_sort(sparks: &[Spark]) -> Vec<&Spark> {
    let mut sorted: Vec<&Spark> = sparks.iter().collect();
    sorted.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| spark_type_rank(&a.spark_type).cmp(&spark_type_rank(&b.spark_type)))
            .then_with(|| status_rank(&a.status).cmp(&status_rank(&b.status)))
            .then_with(|| a.id.cmp(&b.id))
    });
    sorted
}

// ── Status menu ──────────────────────────────────────

/// Inline status popover state. Tracks which spark (if any) currently
/// has its status menu open and whether we're in the close-reason
/// sub-menu. The menu is rendered next to the row in `view_spark_row`.
#[derive(Debug, Clone, Default)]
pub struct StatusMenu {
    pub open_for: Option<String>,
    pub close_stage: bool,
}

impl StatusMenu {
    pub fn dismiss(&mut self) {
        self.open_for = None;
        self.close_stage = false;
    }

    pub fn open(&mut self, spark_id: String) {
        self.open_for = Some(spark_id);
        self.close_stage = false;
    }

    pub fn enter_close_stage(&mut self) {
        self.close_stage = true;
    }

    pub fn is_open_for(&self, spark_id: &str) -> bool {
        self.open_for.as_deref() == Some(spark_id)
    }
}

/// Available close reasons offered when the user chooses "Closed" from
/// the status menu. The first column is the value persisted to
/// `closed_reason`; the second is the human-readable label.
pub const CLOSE_REASONS: &[(&str, &str)] = &[
    ("completed", "Completed"),
    ("obsolete", "Obsolete"),
    ("duplicate", "Duplicate"),
    ("wontfix", "Won't fix"),
];

/// All status options exposed via the popover menu, in display order.
/// `closed` is handled separately because it triggers the close-reason
/// sub-menu rather than a direct status update.
pub const STATUS_OPTIONS: &[(&str, &str)] = &[
    ("open", "Open"),
    ("in_progress", "In Progress"),
    ("blocked", "Blocked"),
    ("deferred", "Deferred"),
];

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
    /// Open the inline status popover for a spark.
    OpenStatusMenu(String),
    /// Dismiss the status popover.
    CloseStatusMenu,
    /// Apply a non-closed status (open / in_progress / blocked / deferred).
    SetStatus(String, String),
    /// Switch the popover into the close-reason sub-menu.
    BeginCloseFlow(String),
    /// Close the spark with a specific reason.
    CloseSparkWithReason(String, String),
}

// ── View ─────────────────────────────────────────────

pub fn view<'a>(
    sparks: &'a [Spark],
    blocked_ids: &'a HashSet<String>,
    pal: &Palette,
    has_bg: bool,
    create_form: &'a CreateForm,
    status_menu: &'a StatusMenu,
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
        // Apply the default (priority, type, status, id) sort before
        // rendering so users can scan for high-priority work. Done in
        // the UI layer so filters and grouping compose with it later.
        for spark in default_sort(sparks) {
            let is_blocked = blocked_ids.contains(&spark.id);
            list = list.push(view_spark_row(spark, is_blocked, &pal, status_menu));
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
        prio_chips = prio_chips.push(form_chip(&format!("P{p}"), selected, &pal, move || {
            Message::CreateFormPriorityChanged(p)
        }));
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
    text(label).size(FONT_LABEL).color(pal.text_tertiary).into()
}

fn form_chip<'a, F>(label: &str, selected: bool, pal: &Palette, on_press: F) -> Element<'a, Message>
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

fn status_symbol(status: &str) -> &'static str {
    match status {
        "open" => "\u{25CB}",        // ○
        "in_progress" => "\u{25D4}", // ◔
        "blocked" => "\u{25A0}",     // ■
        "deferred" => "\u{25CC}",    // ◌
        "closed" => "\u{25CF}",      // ●
        _ => "\u{25CB}",
    }
}

fn view_spark_row<'a>(
    spark: &'a Spark,
    is_blocked: bool,
    pal: &Palette,
    status_menu: &'a StatusMenu,
) -> Element<'a, Message> {
    let pal = *pal;
    let status_indicator = status_symbol(&spark.status);
    let priority_label = format!("P{}", spark.priority);
    let id = spark.id.clone();

    let status_btn = button(
        text(status_indicator)
            .size(FONT_ICON_SM)
            .color(pal.text_secondary),
    )
    .style(button::text)
    .padding([2, 4])
    .on_press(Message::OpenStatusMenu(id.clone()));

    // When a spark has open blockers, dim the title and surface a 🔒-style
    // padlock so agents glance-read "don't claim this" without opening detail.
    let title_color = if is_blocked {
        pal.text_tertiary
    } else {
        pal.text_primary
    };

    let mut row_inner = row![
        text(priority_label)
            .size(FONT_LABEL)
            .color(pal.text_tertiary),
        text(&spark.title).size(FONT_BODY).color(title_color),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    if is_blocked {
        row_inner = row_inner.push(text("\u{1F512}").size(FONT_LABEL).color(pal.text_tertiary));
    }

    let main_row = row![
        status_btn,
        button(row_inner)
            .style(button::text)
            .width(Length::Fill)
            .padding([5, 6])
            .on_press(Message::SelectSpark(id.clone()))
    ]
    .spacing(2)
    .align_y(iced::Alignment::Center);

    if status_menu.is_open_for(&spark.id) {
        column![
            main_row,
            view_status_menu(&spark.id, &spark.status, status_menu.close_stage, &pal),
        ]
        .spacing(2)
        .into()
    } else {
        main_row.into()
    }
}

fn view_status_menu<'a>(
    spark_id: &'a str,
    current_status: &str,
    close_stage: bool,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    let menu_body: Element<Message> = if close_stage {
        let mut chips = row![text("Close as:").size(FONT_SMALL).color(pal.text_tertiary),]
            .spacing(6)
            .align_y(iced::Alignment::Center);
        for (key, label) in CLOSE_REASONS {
            let sid = spark_id.to_string();
            let key_owned = (*key).to_string();
            chips = chips.push(menu_chip(label, false, &pal, move || {
                Message::CloseSparkWithReason(sid.clone(), key_owned.clone())
            }));
        }
        chips.into()
    } else {
        let mut chips = row![].spacing(6).align_y(iced::Alignment::Center);
        for (key, label) in STATUS_OPTIONS {
            let selected = current_status == *key;
            let sid = spark_id.to_string();
            let key_owned = (*key).to_string();
            chips = chips.push(menu_chip(label, selected, &pal, move || {
                Message::SetStatus(sid.clone(), key_owned.clone())
            }));
        }
        let sid_close = spark_id.to_string();
        let closed_selected = current_status == "closed";
        chips = chips.push(menu_chip("Closed", closed_selected, &pal, move || {
            Message::BeginCloseFlow(sid_close.clone())
        }));
        chips.into()
    };

    let cancel_btn = button(text("\u{00D7}").size(FONT_LABEL).color(pal.text_tertiary))
        .style(button::text)
        .padding([2, 6])
        .on_press(Message::CloseStatusMenu);

    let popover = row![menu_body, Space::new().width(Length::Fill), cancel_btn]
        .spacing(8)
        .align_y(iced::Alignment::Center);

    container(popover)
        .padding([6, 8])
        .width(Length::Fill)
        .style(move |_t: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(iced::Color {
                a: 0.06,
                ..pal.text_primary
            })),
            border: iced::Border {
                color: pal.border,
                width: 1.0,
                radius: iced::border::Radius::from(6.0),
            },
            ..Default::default()
        })
        .into()
}

fn menu_chip<'a, F>(label: &str, selected: bool, pal: &Palette, on_press: F) -> Element<'a, Message>
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_spark(id: &str, priority: i32, spark_type: &str, status: &str) -> Spark {
        Spark {
            id: id.to_string(),
            title: String::new(),
            description: String::new(),
            status: status.to_string(),
            priority,
            spark_type: spark_type.to_string(),
            assignee: None,
            owner: None,
            parent_id: None,
            workshop_id: String::new(),
            estimated_minutes: None,
            github_issue_number: None,
            github_repo: None,
            metadata: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            closed_at: None,
            closed_reason: None,
            due_at: None,
            defer_until: None,
            risk_level: None,
            scope_boundary: None,
        }
    }

    #[test]
    fn default_sort_orders_by_priority_then_type_then_status_then_id() {
        // Fixture mixes priorities, types, and statuses. Insertion order
        // deliberately scrambles every sort key so a stable sort alone
        // would not produce the expected output.
        let sparks = vec![
            mk_spark("sp-e", 2, "task", "open"),
            mk_spark("sp-a", 0, "bug", "in_progress"),
            mk_spark("sp-b", 0, "epic", "open"),
            mk_spark("sp-d", 1, "feature", "blocked"),
            mk_spark("sp-c", 0, "bug", "in_progress"),
            mk_spark("sp-f", 2, "task", "open"),
            mk_spark("sp-g", 0, "bug", "blocked"),
            mk_spark("sp-h", 1, "feature", "open"),
        ];

        let sorted = default_sort(&sparks);
        let ids: Vec<&str> = sorted.iter().map(|s| s.id.as_str()).collect();

        // Expected:
        // P0 epic open            -> sp-b
        // P0 bug in_progress      -> sp-a, sp-c (by id)
        // P0 bug blocked          -> sp-g
        // P1 feature blocked      -> sp-d
        // P1 feature open         -> sp-h
        // P2 task open            -> sp-e, sp-f (by id)
        assert_eq!(
            ids,
            vec!["sp-b", "sp-a", "sp-c", "sp-g", "sp-d", "sp-h", "sp-e", "sp-f"]
        );
    }

    #[test]
    fn default_sort_is_deterministic_across_calls() {
        let sparks = vec![
            mk_spark("sp-2", 1, "task", "open"),
            mk_spark("sp-1", 1, "task", "open"),
            mk_spark("sp-3", 0, "bug", "blocked"),
        ];
        let a: Vec<&str> = default_sort(&sparks).iter().map(|s| s.id.as_str()).collect();
        let b: Vec<&str> = default_sort(&sparks).iter().map(|s| s.id.as_str()).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn default_sort_type_order_is_fixed() {
        let sparks = vec![
            mk_spark("a", 0, "milestone", "open"),
            mk_spark("b", 0, "chore", "open"),
            mk_spark("c", 0, "spike", "open"),
            mk_spark("d", 0, "task", "open"),
            mk_spark("e", 0, "feature", "open"),
            mk_spark("f", 0, "bug", "open"),
            mk_spark("g", 0, "epic", "open"),
        ];
        let ids: Vec<&str> = default_sort(&sparks).iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["g", "f", "e", "d", "c", "b", "a"]);
    }

    #[test]
    fn default_sort_status_order_is_fixed() {
        let sparks = vec![
            mk_spark("a", 0, "task", "closed"),
            mk_spark("b", 0, "task", "completed"),
            mk_spark("c", 0, "task", "deferred"),
            mk_spark("d", 0, "task", "open"),
            mk_spark("e", 0, "task", "blocked"),
            mk_spark("f", 0, "task", "in_progress"),
        ];
        let ids: Vec<&str> = default_sort(&sparks).iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["f", "e", "d", "c", "b", "a"]);
    }

    #[test]
    fn default_sort_unknown_type_and_status_sink_to_end() {
        let sparks = vec![
            mk_spark("a", 0, "zzz", "open"),
            mk_spark("b", 0, "task", "open"),
            mk_spark("c", 0, "task", "zzz"),
        ];
        let ids: Vec<&str> = default_sort(&sparks).iter().map(|s| s.id.as_str()).collect();
        // Known type before unknown; within task, known status before unknown.
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn status_menu_dismiss_clears_state() {
        let mut m = StatusMenu::default();
        m.open("sp-1".into());
        m.enter_close_stage();
        assert!(m.is_open_for("sp-1"));
        assert!(m.close_stage);
        m.dismiss();
        assert!(m.open_for.is_none());
        assert!(!m.close_stage);
    }

    #[test]
    fn opening_a_new_menu_resets_close_stage() {
        let mut m = StatusMenu::default();
        m.open("sp-1".into());
        m.enter_close_stage();
        m.open("sp-2".into());
        assert!(m.is_open_for("sp-2"));
        assert!(!m.close_stage);
    }

    #[test]
    fn close_reasons_cover_expected_values() {
        let keys: Vec<&str> = CLOSE_REASONS.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"completed"));
        assert!(keys.contains(&"obsolete"));
        assert!(keys.contains(&"duplicate"));
        assert!(keys.contains(&"wontfix"));
    }

    #[test]
    fn status_options_cover_all_non_closed_states() {
        let keys: Vec<&str> = STATUS_OPTIONS.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec!["open", "in_progress", "blocked", "deferred"]);
    }

    #[test]
    fn status_symbol_distinguishes_states() {
        assert_ne!(status_symbol("open"), status_symbol("in_progress"));
        assert_ne!(status_symbol("blocked"), status_symbol("deferred"));
        assert_ne!(status_symbol("closed"), status_symbol("open"));
    }
}
