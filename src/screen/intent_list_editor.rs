// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Generic row-list editor for the three intent lists rendered in the
//! spark detail view: acceptance criteria, invariants, and non-goals.
//!
//! All three lists share this single widget implementation so a bug fix
//! in one lands in all three (see spark ryve-212c63aa, invariant:
//! "All three lists share one widget implementation").
//!
//! The editor is a thin "view + pure helpers" module:
//!
//! * [`ListKind`] identifies which list a message refers to.
//! * [`IntentListDrafts`] holds the per-spark working copy of all three
//!   lists so inline edits don't round-trip through the database on every
//!   keystroke. The draft is seeded from `Spark::intent()` whenever the
//!   selected spark changes and committed via [`update_spark`] whenever a
//!   structural change (add/delete/move/submit) happens.
//! * [`view`] renders a single list — parameterised by kind, title, label,
//!   and the draft slice.
//! * Pure helpers ([`add_blank`], [`delete_at`], [`move_up`], [`move_down`],
//!   [`prune_blanks`], [`rebuild_metadata`]) contain all of the mutation
//!   logic and are trivially unit-testable.

use data::sparks::types::Spark;
#[cfg(test)]
use data::sparks::types::SparkIntent;
use iced::widget::{Space, button, column, container, row, text, text_input};
use iced::{Element, Length, Theme};

use crate::style::{FONT_BODY, FONT_ICON, FONT_LABEL, FONT_SMALL, Palette};

// ── Types ────────────────────────────────────────────

/// Which intent list a message refers to. Acceptance criteria have their
/// own dedicated edit path (`AcceptanceCriteriaEdit`), so only invariants
/// and non-goals are routed through the shared row-list widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListKind {
    Invariants,
    NonGoals,
}

impl ListKind {
    /// Iteration order — used by tests and any caller that wants to
    /// render both lists.
    #[cfg(test)]
    pub const ALL: [ListKind; 2] = [Self::Invariants, Self::NonGoals];

    pub fn section_title(self) -> &'static str {
        match self {
            Self::Invariants => "Invariants",
            Self::NonGoals => "Non-Goals",
        }
    }

    pub fn add_label(self) -> &'static str {
        match self {
            Self::Invariants => "+ Add invariant",
            Self::NonGoals => "+ Add non-goal",
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Invariants => "Invariant",
            Self::NonGoals => "Non-goal",
        }
    }
}

/// Working copy of the three editable intent lists for whichever spark is
/// currently selected in the detail view. Seeded from `Spark::intent()`
/// and committed back by rewriting `metadata.intent.*` via `update_spark`.
#[derive(Debug, Clone, Default)]
pub struct IntentListDrafts {
    /// The spark id the drafts correspond to — cleared or replaced when
    /// the selection changes so stale edits don't bleed across sparks.
    pub spark_id: Option<String>,
    pub acceptance: Vec<String>,
    pub invariants: Vec<String>,
    pub non_goals: Vec<String>,
}

impl IntentListDrafts {
    /// Rehydrate the drafts from a spark's current persisted intent. Call
    /// this whenever `selected_spark` is set, after a save round-trip, or
    /// when fresh data is loaded from the database.
    pub fn seed_from(&mut self, spark: &Spark) {
        let intent = spark.intent();
        self.spark_id = Some(spark.id.clone());
        self.acceptance = intent.acceptance_criteria;
        self.invariants = intent.invariants;
        self.non_goals = intent.non_goals;
    }

    pub fn clear(&mut self) {
        self.spark_id = None;
        self.acceptance.clear();
        self.invariants.clear();
        self.non_goals.clear();
    }

    pub fn list_mut(&mut self, kind: ListKind) -> &mut Vec<String> {
        match kind {
            ListKind::Invariants => &mut self.invariants,
            ListKind::NonGoals => &mut self.non_goals,
        }
    }
}

// ── Messages ─────────────────────────────────────────

/// Messages emitted by the row-list editor. These are wrapped by
/// `spark_detail::Message::IntentList(_)` and dispatched in `main.rs`.
#[derive(Debug, Clone)]
pub enum Message {
    /// In-flight text change — updates the draft only, no DB write.
    Edit {
        kind: ListKind,
        index: usize,
        value: String,
    },
    /// User pressed Enter on a row — prune blanks and persist.
    Submit { kind: ListKind },
    /// Append a new blank row and focus it.
    Add { kind: ListKind },
    /// Delete the row at `index` and persist.
    Delete { kind: ListKind, index: usize },
    /// Swap the row at `index` with the row above it and persist.
    MoveUp { kind: ListKind, index: usize },
    /// Swap the row at `index` with the row below it and persist.
    MoveDown { kind: ListKind, index: usize },
}

// ── Pure helpers (trivially testable) ────────────────

/// Append an empty row.
pub fn add_blank(list: &mut Vec<String>) {
    list.push(String::new());
}

/// Delete the row at `index` if in range. Returns true on success.
pub fn delete_at(list: &mut Vec<String>, index: usize) -> bool {
    if index < list.len() {
        list.remove(index);
        true
    } else {
        false
    }
}

/// Swap the row at `index` with the one above it. No-op if already at the
/// top. Returns true if a swap happened.
pub fn move_up(list: &mut [String], index: usize) -> bool {
    if index == 0 || index >= list.len() {
        return false;
    }
    list.swap(index - 1, index);
    true
}

/// Swap the row at `index` with the one below it. No-op if already at the
/// bottom. Returns true if a swap happened.
pub fn move_down(list: &mut [String], index: usize) -> bool {
    if index + 1 >= list.len() {
        return false;
    }
    list.swap(index, index + 1);
    true
}

/// Remove rows whose trimmed text is empty. Mirrors the "empty row on
/// blur is auto-deleted" rule from the acceptance-criteria task. Returns
/// the number of rows removed.
pub fn prune_blanks(list: &mut Vec<String>) -> usize {
    let before = list.len();
    list.retain(|s| !s.trim().is_empty());
    before - list.len()
}

/// Merge the three draft lists back into a spark's metadata JSON,
/// producing the new value to pass to `UpdateSpark::metadata`. Preserves
/// any other keys under `"intent"` (problem_statement, verification_summary)
/// and any top-level metadata keys.
pub fn rebuild_metadata(
    current_metadata: &str,
    acceptance: &[String],
    invariants: &[String],
    non_goals: &[String],
) -> String {
    use serde_json::{Map, Value};

    // Filter out blank entries on the way out — the draft may still hold
    // blanks while the user is typing, but we never persist them.
    let clean = |xs: &[String]| -> Vec<String> {
        xs.iter()
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .collect()
    };

    let mut root: Value =
        serde_json::from_str(current_metadata).unwrap_or_else(|_| Value::Object(Map::new()));
    if !root.is_object() {
        root = Value::Object(Map::new());
    }
    let root_obj = root.as_object_mut().expect("root is object");

    // Preserve existing intent sub-object so we don't clobber other keys.
    let intent_val = root_obj
        .remove("intent")
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut intent_obj: Map<String, Value> = match intent_val {
        Value::Object(m) => m,
        _ => Map::new(),
    };

    intent_obj.insert(
        "acceptance_criteria".to_string(),
        serde_json::to_value(clean(acceptance)).unwrap_or(Value::Array(vec![])),
    );
    intent_obj.insert(
        "invariants".to_string(),
        serde_json::to_value(clean(invariants)).unwrap_or(Value::Array(vec![])),
    );
    intent_obj.insert(
        "non_goals".to_string(),
        serde_json::to_value(clean(non_goals)).unwrap_or(Value::Array(vec![])),
    );

    root_obj.insert("intent".to_string(), Value::Object(intent_obj));
    serde_json::to_string(&root).unwrap_or_else(|_| current_metadata.to_string())
}

/// Convenience: parse `intent()` off a metadata JSON string (used by
/// tests to round-trip through [`rebuild_metadata`]).
#[cfg(test)]
pub fn intent_from_metadata(metadata: &str) -> SparkIntent {
    serde_json::from_str::<serde_json::Value>(metadata)
        .ok()
        .and_then(|v| serde_json::from_value(v["intent"].clone()).ok())
        .unwrap_or_default()
}

// ── View ─────────────────────────────────────────────

/// Render a single editable list. `draft` is the working copy on the
/// Workshop — callers pass the slice for whichever list they want drawn.
pub fn view<'a>(kind: ListKind, draft: &'a [String], pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let mut col = column![
        text(kind.section_title())
            .size(FONT_LABEL)
            .color(pal.text_tertiary),
    ]
    .spacing(4);

    for (idx, value) in draft.iter().enumerate() {
        col = col.push(view_row(kind, idx, value, draft.len(), &pal));
    }

    let add_btn = button(text(kind.add_label()).size(FONT_LABEL).color(pal.accent))
        .style(button::text)
        .padding([3, 8])
        .on_press(Message::Add { kind });

    col = col.push(add_btn);
    col.into()
}

fn view_row<'a>(
    kind: ListKind,
    index: usize,
    value: &'a str,
    total: usize,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    // Drag/reorder handle: up + down arrows. Iced doesn't expose native
    // drag-and-drop for column children, so we render the reorder affordance
    // as a stacked pair of arrow buttons — behaviourally equivalent and
    // shared across all three lists.
    let up_color = if index == 0 {
        pal.text_tertiary
    } else {
        pal.accent
    };
    let down_color = if index + 1 >= total {
        pal.text_tertiary
    } else {
        pal.accent
    };

    let mut up_btn = button(text("\u{25B2}").size(FONT_SMALL).color(up_color))
        .style(button::text)
        .padding([1, 4]);
    if index > 0 {
        up_btn = up_btn.on_press(Message::MoveUp { kind, index });
    }

    let mut down_btn = button(text("\u{25BC}").size(FONT_SMALL).color(down_color))
        .style(button::text)
        .padding([1, 4]);
    if index + 1 < total {
        down_btn = down_btn.on_press(Message::MoveDown { kind, index });
    }

    let handle = column![up_btn, down_btn].spacing(0);

    let input = text_input(kind.placeholder(), value)
        .size(FONT_BODY)
        .padding([4, 6])
        .on_input(move |v| Message::Edit {
            kind,
            index,
            value: v,
        })
        .on_submit(Message::Submit { kind });

    let delete_btn = button(text("\u{00D7}").size(FONT_ICON).color(pal.text_tertiary))
        .style(button::text)
        .padding([2, 6])
        .on_press(Message::Delete { kind, index });

    container(
        row![handle, input, delete_btn,]
            .spacing(6)
            .align_y(iced::Alignment::Center),
    )
    .padding([2, 0])
    .width(Length::Fill)
    .style(move |_t: &Theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(iced::Color {
            a: 0.02,
            ..pal.text_primary
        })),
        ..Default::default()
    })
    .into()
}

/// Spacer used by callers that need a vertical gap between sections.
#[allow(dead_code)]
pub fn vspace() -> Element<'static, Message> {
    Space::new().height(4).into()
}

// ── Tests ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> String {
        x.to_string()
    }

    #[test]
    fn add_blank_appends_empty_string() {
        let mut list = vec![s("a")];
        add_blank(&mut list);
        assert_eq!(list, vec![s("a"), s("")]);
    }

    #[test]
    fn delete_at_removes_in_range() {
        let mut list = vec![s("a"), s("b"), s("c")];
        assert!(delete_at(&mut list, 1));
        assert_eq!(list, vec![s("a"), s("c")]);
        // out of range is a no-op
        assert!(!delete_at(&mut list, 5));
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn move_up_swaps_with_previous() {
        let mut list = vec![s("a"), s("b"), s("c")];
        assert!(move_up(&mut list, 2));
        assert_eq!(list, vec![s("a"), s("c"), s("b")]);
        // top row can't move up
        assert!(!move_up(&mut list, 0));
        assert_eq!(list, vec![s("a"), s("c"), s("b")]);
    }

    #[test]
    fn move_down_swaps_with_next() {
        let mut list = vec![s("a"), s("b"), s("c")];
        assert!(move_down(&mut list, 0));
        assert_eq!(list, vec![s("b"), s("a"), s("c")]);
        // bottom row can't move down
        assert!(!move_down(&mut list, 2));
        assert_eq!(list, vec![s("b"), s("a"), s("c")]);
    }

    #[test]
    fn prune_blanks_drops_empty_and_whitespace_rows() {
        let mut list = vec![s("a"), s(""), s("   "), s("b")];
        assert_eq!(prune_blanks(&mut list), 2);
        assert_eq!(list, vec![s("a"), s("b")]);
    }

    #[test]
    fn rebuild_metadata_round_trips_all_three_lists() {
        let md = r#"{"intent":{"problem_statement":"keep me"}}"#;
        let out = rebuild_metadata(
            md,
            &[s("ac1"), s("ac2")],
            &[s("inv1")],
            &[s("ng1"), s("ng2"), s("ng3")],
        );
        let parsed = intent_from_metadata(&out);
        assert_eq!(parsed.acceptance_criteria, vec![s("ac1"), s("ac2")]);
        assert_eq!(parsed.invariants, vec![s("inv1")]);
        assert_eq!(parsed.non_goals, vec![s("ng1"), s("ng2"), s("ng3")]);
        assert_eq!(parsed.problem_statement.as_deref(), Some("keep me"));
    }

    #[test]
    fn rebuild_metadata_strips_blanks_before_persist() {
        let out = rebuild_metadata(
            "{}",
            &[s("ac1"), s("   "), s("ac2")],
            &[s(""), s("inv")],
            &[],
        );
        let parsed = intent_from_metadata(&out);
        assert_eq!(parsed.acceptance_criteria, vec![s("ac1"), s("ac2")]);
        assert_eq!(parsed.invariants, vec![s("inv")]);
        assert!(parsed.non_goals.is_empty());
    }

    #[test]
    fn rebuild_metadata_tolerates_malformed_existing_metadata() {
        let out = rebuild_metadata("not json at all", &[s("ac")], &[], &[]);
        let parsed = intent_from_metadata(&out);
        assert_eq!(parsed.acceptance_criteria, vec![s("ac")]);
    }

    #[test]
    fn rebuild_metadata_preserves_unrelated_top_level_keys() {
        let md = r#"{"other":"keep","intent":{"verification_summary":"vs"}}"#;
        let out = rebuild_metadata(md, &[s("ac")], &[], &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["other"], serde_json::json!("keep"));
        assert_eq!(v["intent"]["verification_summary"], serde_json::json!("vs"));
        assert_eq!(
            v["intent"]["acceptance_criteria"],
            serde_json::json!(["ac"])
        );
    }

    #[test]
    fn list_kind_add_labels_are_distinct_per_kind() {
        // Both kinds must surface a distinct '+ Add ...' button so
        // the UI is unambiguous — invariant for spark ryve-212c63aa.
        let labels: Vec<&str> = ListKind::ALL.iter().map(|k| k.add_label()).collect();
        assert_eq!(labels, vec!["+ Add invariant", "+ Add non-goal"]);
    }

    #[test]
    fn drafts_seed_and_clear() {
        use data::sparks::types::Spark;
        let spark = Spark {
            id: "sp-x".to_string(),
            title: "t".to_string(),
            description: String::new(),
            status: "open".to_string(),
            priority: 2,
            spark_type: "task".to_string(),
            assignee: None,
            owner: None,
            parent_id: None,
            workshop_id: "ws".to_string(),
            estimated_minutes: None,
            github_issue_number: None,
            github_repo: None,
            metadata: r#"{"intent":{"invariants":["one"],"non_goals":["two","three"],"acceptance_criteria":["four"]}}"#.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            closed_at: None,
            closed_reason: None,
            due_at: None,
            defer_until: None,
            risk_level: None,
            scope_boundary: None,
        };
        let mut drafts = IntentListDrafts::default();
        drafts.seed_from(&spark);
        assert_eq!(drafts.spark_id.as_deref(), Some("sp-x"));
        assert_eq!(drafts.acceptance, vec![s("four")]);
        assert_eq!(drafts.invariants, vec![s("one")]);
        assert_eq!(drafts.non_goals, vec![s("two"), s("three")]);
        drafts.clear();
        assert!(drafts.spark_id.is_none());
        assert!(drafts.acceptance.is_empty());
    }
}
