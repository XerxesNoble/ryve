// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Spark detail view — shown when a spark is selected in the workgraph panel.

use data::sparks::types::{Contract, ContractEnforcement, ContractKind, Spark};
use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Theme};

use crate::style::{self, Palette, FONT_BODY, FONT_HEADER, FONT_ICON, FONT_LABEL, FONT_SMALL};

// ── Form state ───────────────────────────────────────

/// Inline create-contract form state. Held on the Workshop so it
/// survives cross-message updates and follows the spark_create_form
/// pattern used elsewhere in the workgraph panel.
#[derive(Debug, Clone)]
pub struct ContractCreateForm {
    pub visible: bool,
    pub kind: ContractKind,
    pub description: String,
    pub check_command: String,
    pub enforcement: ContractEnforcement,
}

impl Default for ContractCreateForm {
    fn default() -> Self {
        Self {
            visible: false,
            kind: ContractKind::CustomCommand,
            description: String::new(),
            check_command: String::new(),
            enforcement: ContractEnforcement::Required,
        }
    }
}

impl ContractCreateForm {
    pub fn reset(&mut self) {
        self.visible = false;
        self.kind = ContractKind::CustomCommand;
        self.description.clear();
        self.check_command.clear();
        self.enforcement = ContractEnforcement::Required;
    }
}

/// Cycle through ContractKind variants for the form picker.
pub fn next_contract_kind(k: ContractKind) -> ContractKind {
    match k {
        ContractKind::CustomCommand => ContractKind::TestPass,
        ContractKind::TestPass => ContractKind::NoApiBreak,
        ContractKind::NoApiBreak => ContractKind::GrepAbsent,
        ContractKind::GrepAbsent => ContractKind::GrepPresent,
        ContractKind::GrepPresent => ContractKind::CustomCommand,
    }
}

/// Toggle enforcement.
pub fn toggle_enforcement(e: ContractEnforcement) -> ContractEnforcement {
    match e {
        ContractEnforcement::Required => ContractEnforcement::Advisory,
        ContractEnforcement::Advisory => ContractEnforcement::Required,
    }
}

fn contract_kind_label(k: ContractKind) -> &'static str {
    match k {
        ContractKind::CustomCommand => "custom_command",
        ContractKind::TestPass => "test_pass",
        ContractKind::NoApiBreak => "no_api_break",
        ContractKind::GrepAbsent => "grep_absent",
        ContractKind::GrepPresent => "grep_present",
    }
}

fn enforcement_label(e: ContractEnforcement) -> &'static str {
    match e {
        ContractEnforcement::Required => "required",
        ContractEnforcement::Advisory => "advisory",
    }
}

// ── Messages ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    Back,
    /// Quick status cycle: pass (spark_id, new_status)
    CycleStatus(String, String),
    /// Show inline create-contract form for the given spark.
    ShowCreateContract,
    /// Cancel the create-contract form.
    CancelCreateContract,
    /// Cycle the kind in the create form.
    CycleContractKind,
    /// Toggle enforcement in the create form.
    ToggleContractEnforcement,
    ContractDescriptionChanged(String),
    ContractCheckCommandChanged(String),
    /// Submit the create form for the given spark id.
    SubmitContract(String),
    /// Run the check command for a contract id (and spark id, for refresh).
    RunContract { spark_id: String, contract_id: i64 },
    /// Delete a contract by id (and spark id, for refresh).
    DeleteContract { spark_id: String, contract_id: i64 },
}

// ── View ─────────────────────────────────────────────

pub fn view<'a>(
    spark: &'a Spark,
    contracts: &'a [Contract],
    create_form: &'a ContractCreateForm,
    pal: &Palette,
    has_bg: bool,
) -> Element<'a, Message> {
    let pal = *pal;

    // Back button + header row
    let back_btn = button(
        row![
            text("\u{2190}").size(FONT_ICON).color(pal.accent),
            text("Back").size(FONT_LABEL).color(pal.accent),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    )
    .style(button::text)
    .padding([4, 8])
    .on_press(Message::Back);

    let header = row![back_btn, Space::new().width(Length::Fill)]
        .spacing(4)
        .padding([8, 10]);

    // Title
    let title = text(&spark.title)
        .size(FONT_HEADER + 4.0)
        .color(pal.text_primary);

    let title_row = container(title).padding([4, 10]);

    // Status / Priority / Type badges
    let status_indicator = status_symbol(&spark.status);
    let status_color = status_color(&spark.status, &pal);
    let next = next_status_str(&spark.status);

    let status_pill = button(
        row![
            text(status_indicator).size(FONT_LABEL).color(status_color),
            text(format_status(&spark.status))
                .size(FONT_LABEL)
                .color(status_color),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    )
    .style(button::text)
    .padding([3, 8])
    .on_press(Message::CycleStatus(
        spark.id.clone(),
        next.to_string(),
    ));

    let priority_color = priority_color(spark.priority, &pal);
    let priority_pill = container(
        text(format!("P{}", spark.priority))
            .size(FONT_LABEL)
            .color(priority_color),
    )
    .padding([3, 8]);

    let type_pill = container(
        text(&spark.spark_type)
            .size(FONT_LABEL)
            .color(pal.text_secondary),
    )
    .padding([3, 8]);

    let badges = row![status_pill, priority_pill, type_pill]
        .spacing(6)
        .padding([4, 10])
        .align_y(iced::Alignment::Center);

    // Separator
    let sep = container(Space::new().height(1))
        .width(Length::Fill)
        .padding([0, 10])
        .style(move |_theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(pal.separator)),
            ..Default::default()
        });

    // Description
    let mut body = column![].spacing(12).padding([8, 10]);

    if !spark.description.is_empty() {
        body = body.push(
            column![
                text("Description")
                    .size(FONT_LABEL)
                    .color(pal.text_tertiary),
                text(&spark.description)
                    .size(FONT_BODY)
                    .color(pal.text_primary),
            ]
            .spacing(4),
        );
    }

    // Intent section — intent() returns an owned struct, so we extract
    // owned strings to avoid lifetime issues with the view tree.
    let intent = spark.intent();

    if let Some(problem) = intent.problem_statement {
        if !problem.is_empty() {
            body = body.push(
                column![
                    text("Problem Statement")
                        .size(FONT_LABEL)
                        .color(pal.text_tertiary),
                    text(problem).size(FONT_BODY).color(pal.text_primary),
                ]
                .spacing(4),
            );
        }
    }

    if !intent.invariants.is_empty() {
        let mut items = column![
            text("Invariants")
                .size(FONT_LABEL)
                .color(pal.text_tertiary),
        ]
        .spacing(2);
        for inv in intent.invariants {
            items = items.push(
                text(format!("\u{2022} {inv}"))
                    .size(FONT_BODY)
                    .color(pal.text_primary),
            );
        }
        body = body.push(items);
    }

    if !intent.non_goals.is_empty() {
        let mut items = column![
            text("Non-Goals")
                .size(FONT_LABEL)
                .color(pal.text_tertiary),
        ]
        .spacing(2);
        for ng in intent.non_goals {
            items = items.push(
                text(format!("\u{2022} {ng}"))
                    .size(FONT_BODY)
                    .color(pal.text_primary),
            );
        }
        body = body.push(items);
    }

    if !intent.acceptance_criteria.is_empty() {
        let mut items = column![
            text("Acceptance Criteria")
                .size(FONT_LABEL)
                .color(pal.text_tertiary),
        ]
        .spacing(2);
        for ac in intent.acceptance_criteria {
            items = items.push(
                text(format!("\u{2022} {ac}"))
                    .size(FONT_BODY)
                    .color(pal.text_primary),
            );
        }
        body = body.push(items);
    }

    // ── Contracts section ────────────────────────────
    body = body.push(view_contracts_section(
        &spark.id,
        contracts,
        create_form,
        &pal,
    ));

    // Metadata row: assignee, owner, dates
    let mut meta = column![].spacing(4).padding([8, 0]);

    if let Some(ref assignee) = spark.assignee {
        meta = meta.push(
            row![
                text("Assignee").size(FONT_SMALL).color(pal.text_tertiary),
                text(assignee).size(FONT_SMALL).color(pal.text_secondary),
            ]
            .spacing(8),
        );
    }

    if let Some(ref owner) = spark.owner {
        meta = meta.push(
            row![
                text("Owner").size(FONT_SMALL).color(pal.text_tertiary),
                text(owner).size(FONT_SMALL).color(pal.text_secondary),
            ]
            .spacing(8),
        );
    }

    meta = meta.push(
        row![
            text("Created").size(FONT_SMALL).color(pal.text_tertiary),
            text(&spark.created_at)
                .size(FONT_SMALL)
                .color(pal.text_secondary),
        ]
        .spacing(8),
    );

    meta = meta.push(
        row![
            text("Updated").size(FONT_SMALL).color(pal.text_tertiary),
            text(&spark.updated_at)
                .size(FONT_SMALL)
                .color(pal.text_secondary),
        ]
        .spacing(8),
    );

    body = body.push(meta);

    let content = column![
        header,
        title_row,
        badges,
        sep,
        scrollable(body).height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_theme: &Theme| style::glass_panel(&pal, has_bg))
        .into()
}

// ── Contracts section ────────────────────────────────

fn view_contracts_section<'a>(
    spark_id: &'a str,
    contracts: &'a [Contract],
    form: &'a ContractCreateForm,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    let header = row![
        text("Contracts").size(FONT_LABEL).color(pal.text_tertiary),
        Space::new().width(Length::Fill),
        button(text("+").size(FONT_ICON).color(pal.accent))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::ShowCreateContract),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center);

    let mut col = column![header].spacing(6);

    if form.visible {
        col = col.push(view_create_form(spark_id, form, &pal));
    }

    if contracts.is_empty() && !form.visible {
        col = col.push(
            text("No contracts yet")
                .size(FONT_SMALL)
                .color(pal.text_tertiary),
        );
    } else {
        for c in contracts {
            col = col.push(view_contract_row(spark_id, c, &pal));
        }
    }

    col.into()
}

fn view_create_form<'a>(
    spark_id: &'a str,
    form: &'a ContractCreateForm,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    let kind_btn = button(
        row![
            text("kind:").size(FONT_SMALL).color(pal.text_tertiary),
            text(contract_kind_label(form.kind))
                .size(FONT_LABEL)
                .color(pal.accent),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    )
    .style(button::text)
    .padding([2, 6])
    .on_press(Message::CycleContractKind);

    let enforcement_btn = button(
        row![
            text("enforcement:").size(FONT_SMALL).color(pal.text_tertiary),
            text(enforcement_label(form.enforcement))
                .size(FONT_LABEL)
                .color(match form.enforcement {
                    ContractEnforcement::Required => pal.danger,
                    ContractEnforcement::Advisory => pal.text_secondary,
                }),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    )
    .style(button::text)
    .padding([2, 6])
    .on_press(Message::ToggleContractEnforcement);

    let description_input = text_input("Description...", &form.description)
        .size(FONT_BODY)
        .padding([6, 8])
        .on_input(Message::ContractDescriptionChanged);

    let check_input = text_input("Check command (e.g. 'cargo test')", &form.check_command)
        .size(FONT_BODY)
        .padding([6, 8])
        .on_input(Message::ContractCheckCommandChanged)
        .on_submit(Message::SubmitContract(spark_id.to_string()));

    let submit_btn = button(text("Create").size(FONT_LABEL).color(pal.accent))
        .style(button::text)
        .padding([3, 8])
        .on_press(Message::SubmitContract(spark_id.to_string()));

    let cancel_btn = button(
        text("Cancel")
            .size(FONT_LABEL)
            .color(pal.text_tertiary),
    )
    .style(button::text)
    .padding([3, 8])
    .on_press(Message::CancelCreateContract);

    let actions = row![submit_btn, cancel_btn].spacing(8);

    column![
        row![kind_btn, enforcement_btn]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        description_input,
        check_input,
        actions,
    ]
    .spacing(4)
    .padding([4, 0])
    .into()
}

fn view_contract_row<'a>(
    spark_id: &'a str,
    c: &'a Contract,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    let (status_icon, status_color) = contract_status_visuals(&c.status, &pal);

    let kind_text = text(c.kind.as_str())
        .size(FONT_SMALL)
        .color(pal.text_tertiary);

    let enforcement_color = if c.enforcement == "required" {
        pal.danger
    } else {
        pal.text_tertiary
    };
    let enforcement_text = text(c.enforcement.as_str())
        .size(FONT_SMALL)
        .color(enforcement_color);

    let desc_text = text(c.description.as_str())
        .size(FONT_BODY)
        .color(pal.text_primary);

    let mut actions = row![].spacing(4).align_y(iced::Alignment::Center);

    if c.check_command.as_deref().unwrap_or("").trim().is_empty() {
        // No command: show a disabled "Run" placeholder for clarity.
        actions = actions.push(
            text("\u{25B6}")
                .size(FONT_ICON)
                .color(pal.text_tertiary),
        );
    } else {
        actions = actions.push(
            button(text("\u{25B6} Run").size(FONT_LABEL).color(pal.accent))
                .style(button::text)
                .padding([2, 6])
                .on_press(Message::RunContract {
                    spark_id: spark_id.to_string(),
                    contract_id: c.id,
                }),
        );
    }

    actions = actions.push(
        button(text("\u{00D7}").size(FONT_ICON).color(pal.text_tertiary))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::DeleteContract {
                spark_id: spark_id.to_string(),
                contract_id: c.id,
            }),
    );

    let header = row![
        text(status_icon).size(FONT_LABEL).color(status_color),
        kind_text,
        enforcement_text,
        Space::new().width(Length::Fill),
        actions,
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let mut col = column![header, desc_text].spacing(2);

    if let Some(cmd) = c.check_command.as_deref() {
        if !cmd.is_empty() {
            col = col.push(
                text(format!("$ {cmd}"))
                    .size(FONT_SMALL)
                    .color(pal.text_secondary),
            );
        }
    }

    if let Some(ref when) = c.last_checked_at {
        col = col.push(
            text(format!("last checked {when}"))
                .size(FONT_SMALL)
                .color(pal.text_tertiary),
        );
    }

    container(col)
        .padding([6, 8])
        .width(Length::Fill)
        .style(move |_t: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(iced::Color {
                a: 0.04,
                ..pal.text_primary
            })),
            border: iced::Border {
                radius: 4.0.into(),
                width: 0.0,
                color: iced::Color::TRANSPARENT,
            },
            ..Default::default()
        })
        .into()
}

fn contract_status_visuals(status: &str, pal: &Palette) -> (&'static str, iced::Color) {
    match status {
        "pass" => ("\u{25CF}", iced::Color {
            r: 0.298,
            g: 0.851,
            b: 0.392,
            a: 1.0,
        }),
        "fail" => ("\u{25CF}", pal.danger),
        "skipped" => ("\u{25CC}", pal.text_tertiary),
        _ /* pending */ => ("\u{25CB}", pal.text_secondary),
    }
}

// ── Helpers ──────────────────────────────────────────

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

fn status_color(status: &str, pal: &Palette) -> iced::Color {
    match status {
        "open" => pal.text_secondary,
        "in_progress" => pal.accent,
        "blocked" => pal.danger,
        "deferred" => pal.text_tertiary,
        "closed" => pal.text_tertiary,
        _ => pal.text_secondary,
    }
}

fn format_status(status: &str) -> &'static str {
    match status {
        "open" => "Open",
        "in_progress" => "In Progress",
        "blocked" => "Blocked",
        "deferred" => "Deferred",
        "closed" => "Closed",
        _ => "Unknown",
    }
}

fn priority_color(priority: i32, pal: &Palette) -> iced::Color {
    match priority {
        0 => pal.danger,        // P0 — critical
        1 => iced::Color {      // P1 — orange-ish
            r: 1.0,
            g: 0.6,
            b: 0.0,
            a: 1.0,
        },
        2 => pal.accent,       // P2 — normal
        3 => pal.text_secondary, // P3 — low
        _ => pal.text_tertiary,  // P4+ — minimal
    }
}

/// Cycle: open -> in_progress -> closed -> open
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_kind_round_trips() {
        let mut k = ContractKind::CustomCommand;
        for _ in 0..5 {
            k = next_contract_kind(k);
        }
        assert_eq!(k, ContractKind::CustomCommand);
    }

    #[test]
    fn toggle_enforcement_round_trips() {
        let e = ContractEnforcement::Required;
        assert_eq!(toggle_enforcement(e), ContractEnforcement::Advisory);
        assert_eq!(
            toggle_enforcement(toggle_enforcement(e)),
            ContractEnforcement::Required
        );
    }

    #[test]
    fn create_form_default_is_hidden_required_custom_command() {
        let f = ContractCreateForm::default();
        assert!(!f.visible);
        assert_eq!(f.kind, ContractKind::CustomCommand);
        assert_eq!(f.enforcement, ContractEnforcement::Required);
    }

    #[test]
    fn create_form_reset_clears_fields() {
        let mut f = ContractCreateForm {
            visible: true,
            kind: ContractKind::TestPass,
            description: "do the thing".into(),
            check_command: "cargo test".into(),
            enforcement: ContractEnforcement::Advisory,
        };
        f.reset();
        assert!(!f.visible);
        assert_eq!(f.kind, ContractKind::CustomCommand);
        assert!(f.description.is_empty());
        assert!(f.check_command.is_empty());
        assert_eq!(f.enforcement, ContractEnforcement::Required);
    }

    #[test]
    fn contract_status_visuals_distinguishes_states() {
        let pal = Palette::dark();
        let (pass_icon, _) = contract_status_visuals("pass", &pal);
        let (fail_icon, _) = contract_status_visuals("fail", &pal);
        let (pending_icon, _) = contract_status_visuals("pending", &pal);
        assert_eq!(pass_icon, "\u{25CF}");
        assert_eq!(fail_icon, "\u{25CF}");
        assert_eq!(pending_icon, "\u{25CB}");
    }
}
