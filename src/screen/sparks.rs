// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Workgraph panel — displays and manages sparks for the active workshop.

use std::collections::{HashMap, HashSet};

use data::sparks::types::Spark;
use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Theme};

use crate::style::{
    self, FONT_BODY, FONT_HEADER, FONT_ICON, FONT_ICON_SM, FONT_LABEL, FONT_SMALL, Palette,
};

// ── Filter state ────────────────────────────────────────

/// All status values that can appear as filter pills.
pub const FILTER_STATUSES: &[(&str, &str)] = &[
    ("open", "Open"),
    ("in_progress", "In Progress"),
    ("blocked", "Blocked"),
    ("deferred", "Deferred"),
    ("completed", "Completed"),
];

/// Filter state for the sparks panel. The `status` set holds statuses
/// the user has explicitly selected; an empty set means "show all".
/// `show_closed` is a separate toggle because closed sparks are hidden
/// by default regardless of the status filter.
///
/// Invariant: pill state is a direct mirror of this struct — no separate
/// UI-only flag.
#[derive(Debug, Clone, Default)]
pub struct SparksFilter {
    pub status: HashSet<String>,
    pub show_closed: bool,
}

impl SparksFilter {
    /// Toggle a status in the filter. Returns `true` if the status is now
    /// selected.
    pub fn toggle_status(&mut self, status: &str) -> bool {
        if !self.status.remove(status) {
            self.status.insert(status.to_string());
            true
        } else {
            false
        }
    }

    /// Whether a spark should be visible given the current filter state.
    pub fn matches(&self, spark_status: &str) -> bool {
        // Closed sparks obey their own toggle regardless of the status set.
        if spark_status == "closed" {
            return self.show_closed;
        }
        // Completed is treated like closed when show_closed is false.
        if spark_status == "completed" && !self.show_closed && self.status.is_empty() {
            return true;
        }
        // Empty status set means "show all (minus closed unless toggled)".
        if self.status.is_empty() {
            return true;
        }
        self.status.contains(spark_status)
    }
}

// ── State ────────────────────────────────────────────

/// Inline create form state, held on the Workshop. The form enforces a
/// minimum set of fields before submission: title, type, priority, and
/// (when the type is not `epic`) a parent epic to nest the new spark
/// under. Intent fields (problem, invariants, acceptance) are intentionally
/// absent — they are edited from the spark detail panel after creation.
#[derive(Debug, Clone, Default)]
pub struct CreateForm {
    pub title: String,
    pub spark_type: String,
    pub priority: i32,
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
        self.parent_epic_id = None;
        self.error = None;
    }

    /// Open the form with an optional pre-selected parent epic. Used by
    /// the "+" button handler so that the parent picker defaults to the
    /// focused spark's nearest epic ancestor when available.
    pub fn open_with_default_parent(&mut self, default_parent: Option<String>) {
        self.reset();
        self.parent_epic_id = default_parent;
        self.visible = true;
    }

    /// Validate the form and return the first missing-field error, if
    /// any. `Ok(())` means the form is safe to submit.
    ///
    /// The rules here MUST mirror the data-layer `create_spark` invariant
    /// (see spark ryve-6bc1c9cc): non-epic sparks require a parent, epics
    /// may be top-level. If the data layer ever tightens, this must
    /// tighten in lockstep.
    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("Title is required.".to_string());
        }
        if self.spark_type.is_empty() {
            return Err("Pick a spark type.".to_string());
        }
        if self.spark_type != "epic" && self.parent_epic_id.is_none() {
            return Err("Pick a parent epic (only epics may be top-level).".to_string());
        }
        Ok(())
    }

    /// Cheap wrapper used by the view to decide whether the Submit button
    /// should be enabled. Kept in sync with `validate` — any rule added
    /// there automatically gates the button.
    pub fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }
}

/// Resolve the parent epic id to pre-fill when the "+" button is clicked.
///
/// - If nothing is focused, return `None` (user must pick, unless creating
///   an epic).
/// - If the focused spark is itself an epic, return its id (new spark
///   nests directly under it).
/// - Otherwise walk `parent_id` links upward until we find an epic and
///   return it. If the chain contains no epic (shouldn't happen in a
///   well-formed workgraph, but may during migration), return `None`.
pub fn resolve_default_parent_epic(sparks: &[Spark], focused_id: Option<&str>) -> Option<String> {
    let focused_id = focused_id?;
    // Quick index by id so the walk is O(depth) not O(n*depth).
    let index: HashMap<&str, &Spark> = sparks.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut cursor = index.get(focused_id)?;
    // Guard against cycles or unusually deep chains.
    for _ in 0..64 {
        if cursor.spark_type == "epic" {
            return Some(cursor.id.clone());
        }
        let parent_id = cursor.parent_id.as_deref()?;
        cursor = index.get(parent_id)?;
    }
    None
}

// ── Default sort ─────────────────────────────────────
// NOTE: default_sort is a pure helper kept available for upcoming
// refactors to the render pipeline (sorted grouping). Currently the
// view() path uses group_by_epic on the raw slice; sort integration is
// tracked as follow-up work. Silence dead-code warnings until then.

/// Canonical ordering of `spark_type` values for the default sort. Any
/// value not in this list sorts after every listed type but stably
/// relative to other unknown types (via the string tiebreaker).
#[allow(dead_code)]
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
#[allow(dead_code)]
const STATUS_ORDER: &[&str] = &[
    "in_progress",
    "blocked",
    "open",
    "deferred",
    "completed",
    "closed",
];

#[allow(dead_code)]
fn spark_type_rank(ty: &str) -> usize {
    SPARK_TYPE_ORDER
        .iter()
        .position(|t| *t == ty)
        .unwrap_or(SPARK_TYPE_ORDER.len())
}

#[allow(dead_code)]
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
#[allow(dead_code)]
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

// ── Epic grouping ────────────────────────────────────

/// A single group in the sparks panel: an epic row with its direct
/// children (tasks, bugs, and nested epics) nested underneath. Borrows
/// from the input slice so callers control ownership.
#[derive(Debug)]
pub struct EpicGroup<'a> {
    pub epic: &'a Spark,
    pub children: Vec<&'a Spark>,
}

/// Build per-epic groups from a (pre-sorted) flat spark vec.
///
/// Every epic in `sparks` becomes exactly one `EpicGroup`, in the order
/// the epics appear in the input. Non-epic sparks are appended to the
/// group of their parent epic, preserving input order so the upstream
/// default sort flows through unchanged. Nested epics also appear in
/// their parent epic's `children` so the renderer can place them inline.
///
/// Non-epic sparks whose `parent_id` is missing or does not point at a
/// known epic are dropped — the workgraph layer (spark ryve-b41f60dd)
/// guarantees every non-epic has an epic parent, and silently dropping
/// a malformed row is preferable to rendering it orphaned at top level
/// (which would violate the "no child outside its parent group" invariant).
pub fn group_by_epic<'a>(sparks: &'a [Spark]) -> Vec<EpicGroup<'a>> {
    let mut groups: Vec<EpicGroup<'a>> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for s in sparks {
        if s.spark_type == "epic" {
            index.insert(s.id.as_str(), groups.len());
            groups.push(EpicGroup {
                epic: s,
                children: Vec::new(),
            });
        }
    }
    for s in sparks {
        let Some(pid) = s.parent_id.as_deref() else {
            continue;
        };
        if pid == s.id {
            continue;
        }
        if let Some(&i) = index.get(pid) {
            groups[i].children.push(s);
        }
    }
    groups
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
    ShowReleases,
    ShowCreateForm,
    CreateFormTitleChanged(String),
    CreateFormTypeChanged(String),
    CreateFormPriorityChanged(i32),
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
    /// Toggle a status in the filter pill row.
    ToggleStatusFilter(String),
    /// Toggle visibility of closed sparks.
    ToggleShowClosed,
    /// Toggle the collapsed/expanded state of an epic group in the
    /// workgraph panel. The workshop persists the new state to
    /// `.ryve/ui_state.json` so the decision survives restart.
    /// Sparks ryve-8be256a8 / ryve-926870a9.
    ToggleEpicCollapse(String),
}

// ── Refresh button glyph ─────────────────────────────

/// Pick the Workgraph panel's refresh-button glyph based on whether an
/// explicit Refresh refetch is currently in flight. Pulled out of `view`
/// so it can be unit-tested without constructing an iced `Element`.
/// Spark ryve-7805b38b.
pub(crate) fn refresh_button_glyph(refreshing: bool) -> &'static str {
    if refreshing {
        // Horizontal ellipsis — signals "work in progress".
        "\u{2026}"
    } else {
        // Clockwise open-circle arrow — the normal refresh icon.
        "\u{21BB}"
    }
}

// ── View ─────────────────────────────────────────────

/// Bundles the view parameters to stay within the clippy argument limit.
pub struct ViewCtx<'a> {
    pub sparks: &'a [Spark],
    pub blocked_ids: &'a HashSet<String>,
    pub pal: Palette,
    pub has_bg: bool,
    pub create_form: &'a CreateForm,
    pub status_menu: &'a StatusMenu,
    pub collapsed: &'a HashSet<String>,
    pub refreshing: bool,
    pub filter: &'a SparksFilter,
}

/// Map a spark status to its canonical palette color.
pub fn status_color(status: &str, pal: &Palette) -> iced::Color {
    match status {
        "open" => pal.text_secondary,
        "in_progress" => pal.accent,
        "blocked" => pal.danger,
        "deferred" => pal.text_tertiary,
        "completed" => pal.success,
        "closed" => pal.text_tertiary,
        _ => pal.text_secondary,
    }
}

pub fn view(ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let ViewCtx {
        sparks,
        blocked_ids,
        pal,
        has_bg,
        create_form,
        status_menu,
        collapsed,
        refreshing,
        filter,
    } = ctx;

    // Refresh button: dim the glyph and swap it for an ellipsis while a
    // refetch is in flight so the click surfaces visible feedback.
    // Spark ryve-7805b38b.
    let refresh_glyph = refresh_button_glyph(refreshing);
    let refresh_color = if refreshing {
        pal.text_tertiary
    } else {
        pal.text_secondary
    };
    let header = row![
        text("Workgraph").size(FONT_HEADER).color(pal.text_primary),
        Space::new().width(Length::Fill),
        button(text("Releases").size(FONT_LABEL).color(pal.text_secondary))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::ShowReleases),
        button(text("+").size(FONT_ICON).color(pal.accent))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::ShowCreateForm),
        button(text(refresh_glyph).size(FONT_ICON).color(refresh_color))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::Refresh),
    ]
    .spacing(4)
    .padding([8, 10]);

    // ── Filter pills ──
    let filter_row = view_filter_pills(filter, &pal);

    let mut list = column![].spacing(2).padding([0, 10]);

    // Inline create form (uses unfiltered sparks for epic picker).
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
        let all_groups = group_by_epic(sparks);
        // Apply filter: keep only groups whose epic or children match.
        let groups: Vec<EpicGroup<'_>> = all_groups
            .into_iter()
            .filter_map(|mut g| {
                g.children.retain(|c| filter.matches(&c.status));
                if filter.matches(&g.epic.status) || !g.children.is_empty() {
                    Some(g)
                } else {
                    None
                }
            })
            .collect();
        let epic_ids: HashSet<&str> = groups.iter().map(|g| g.epic.id.as_str()).collect();
        // Top-level groups are those whose epic has no epic-group parent;
        // nested epics are rendered inline under their parent group instead.
        for g in &groups {
            let is_nested = g
                .epic
                .parent_id
                .as_deref()
                .map(|pid| epic_ids.contains(pid))
                .unwrap_or(false);
            if is_nested {
                continue;
            }
            list = list.push(view_epic_group(
                g,
                &groups,
                0,
                blocked_ids,
                &pal,
                status_menu,
                collapsed,
            ));
        }
    }

    let content = column![header, filter_row, scrollable(list).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_theme: &Theme| style::glass_panel(&pal, has_bg))
        .into()
}

// ── Filter pills view ───────────────────────────────

fn view_filter_pills<'a>(filter: &SparksFilter, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let mut pills = row![].spacing(4).align_y(iced::Alignment::Center);

    for (key, label) in FILTER_STATUSES {
        let selected = filter.status.contains(*key);
        let color = status_color(key, &pal);
        let key_owned = (*key).to_string();
        pills = pills.push(filter_pill(label, selected, color, &pal, move || {
            Message::ToggleStatusFilter(key_owned.clone())
        }));
    }

    // "Closed" pill at the end — separate toggle.
    pills = pills.push(filter_pill(
        "Closed",
        filter.show_closed,
        status_color("closed", &pal),
        &pal,
        || Message::ToggleShowClosed,
    ));

    container(pills).padding([2, 10]).width(Length::Fill).into()
}

fn filter_pill<'a, F>(
    label: &str,
    selected: bool,
    active_color: iced::Color,
    pal: &Palette,
    on_press: F,
) -> Element<'a, Message>
where
    F: 'a + Fn() -> Message,
{
    let pal = *pal;
    let (text_color, bg) = if selected {
        (pal.window_bg, active_color)
    } else {
        // Dimmed: use the status color at reduced opacity for the text.
        (
            iced::Color {
                a: 0.45,
                ..active_color
            },
            pal.surface,
        )
    };
    button(text(label.to_string()).size(FONT_LABEL).color(text_color))
        .style(move |_t: &Theme, _s| button::Style {
            background: Some(iced::Background::Color(bg)),
            text_color,
            border: iced::Border {
                color: if selected { active_color } else { pal.border },
                width: 1.0,
                radius: iced::border::Radius::from(10.0),
            },
            ..button::Style::default()
        })
        .padding([2, 8])
        .on_press_with(on_press)
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

    // ── error banner ──
    let error_banner: Element<Message> = if let Some(err) = &form.error {
        text(err.as_str()).size(FONT_SMALL).color(pal.danger).into()
    } else {
        Space::new().height(0).into()
    };

    // Submit button is gated on `is_valid()` — passing `None` to
    // `on_press_maybe` renders the button as disabled so the user cannot
    // attempt to persist an invalid spark. This keeps the UI mirror of
    // the data-layer invariant tight: you can't even *try* to submit an
    // orphan non-epic from the panel.
    let submit_msg = if form.is_valid() {
        Some(Message::SubmitNewSpark)
    } else {
        None
    };
    let submit_color = if form.is_valid() {
        pal.accent
    } else {
        pal.text_tertiary
    };

    let actions = row![
        button(text("Create").size(FONT_LABEL).color(submit_color))
            .style(button::text)
            .padding([3, 8])
            .on_press_maybe(submit_msg),
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

/// Render an epic group: a chevron-prefixed header row plus, when the
/// group is expanded, every direct child indented underneath. Nested
/// epics recurse once so `epic > epic > task` renders the inner epic as
/// its own collapsible group; beyond two levels deep we flatten the
/// remaining rows (per the spark non-goal).
fn view_epic_group<'a>(
    group: &EpicGroup<'a>,
    all_groups: &[EpicGroup<'a>],
    depth: usize,
    blocked_ids: &HashSet<String>,
    pal: &Palette,
    status_menu: &'a StatusMenu,
    collapsed: &'a HashSet<String>,
) -> Element<'a, Message> {
    let pal = *pal;
    let epic = group.epic;
    let is_collapsed = collapsed.contains(&epic.id);
    let is_blocked = blocked_ids.contains(&epic.id);

    let mut col = column![view_epic_header(
        epic,
        is_collapsed,
        is_blocked,
        depth,
        &pal
    )]
    .spacing(2);

    if !is_collapsed {
        for child in &group.children {
            if child.spark_type == "epic"
                && depth < 1
                && let Some(nested) = all_groups.iter().find(|g| g.epic.id == child.id)
            {
                col = col.push(view_epic_group(
                    nested,
                    all_groups,
                    depth + 1,
                    blocked_ids,
                    &pal,
                    status_menu,
                    collapsed,
                ));
                continue;
            }
            let child_blocked = blocked_ids.contains(&child.id);
            col = col.push(view_spark_row_indented(
                child,
                child_blocked,
                depth + 1,
                &pal,
                status_menu,
            ));
        }
    }

    col.into()
}

/// Header row for an `EpicGroup`: a clickable chevron that toggles the
/// collapse state, followed by the same status button + title the task
/// rows use. Indent by `depth` so nested epics sit under their parent.
fn view_epic_header<'a>(
    epic: &'a Spark,
    is_collapsed: bool,
    is_blocked: bool,
    depth: usize,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;
    let chevron = if is_collapsed { "\u{25B8}" } else { "\u{25BE}" };
    let chevron_btn = button(text(chevron).size(FONT_ICON_SM).color(pal.text_secondary))
        .style(button::text)
        .padding([2, 4])
        .on_press(Message::ToggleEpicCollapse(epic.id.clone()));

    let status_indicator = status_symbol(&epic.status);
    let badge_color = style::status_color(&epic.status, &pal);
    let id = epic.id.clone();
    let status_btn = button(text(status_indicator).size(FONT_ICON_SM).color(badge_color))
        .style(button::text)
        .padding([2, 4])
        .on_press(Message::OpenStatusMenu(id.clone()));

    let title_color = if is_blocked {
        pal.text_tertiary
    } else {
        pal.text_primary
    };

    let mut row_inner = row![
        text(format!("P{}", epic.priority))
            .size(FONT_LABEL)
            .color(pal.text_tertiary),
        text(&epic.title).size(FONT_BODY).color(title_color),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    if is_blocked {
        row_inner = row_inner.push(text("\u{1F512}").size(FONT_LABEL).color(pal.text_tertiary));
    }

    let indent = Space::new().width(Length::Fixed(16.0 * depth as f32));

    row![
        indent,
        chevron_btn,
        status_btn,
        button(row_inner)
            .style(button::text)
            .width(Length::Fill)
            .padding([5, 6])
            .on_press(Message::SelectSpark(id)),
    ]
    .spacing(2)
    .align_y(iced::Alignment::Center)
    .into()
}

/// Indented variant of `view_spark_row` for children nested under an
/// epic header. `depth` is a row-level indent in steps of 16 px.
fn view_spark_row_indented<'a>(
    spark: &'a Spark,
    is_blocked: bool,
    depth: usize,
    pal: &Palette,
    status_menu: &'a StatusMenu,
) -> Element<'a, Message> {
    let indent = Space::new().width(Length::Fixed(16.0 * depth as f32));
    row![indent, view_spark_row(spark, is_blocked, pal, status_menu)]
        .spacing(0)
        .align_y(iced::Alignment::Center)
        .width(Length::Fill)
        .into()
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

    let badge_color = style::status_color(&spark.status, &pal);
    let status_btn = button(text(status_indicator).size(FONT_ICON_SM).color(badge_color))
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

    fn mk_sort_spark(id: &str, priority: i32, spark_type: &str, status: &str) -> Spark {
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
            mk_sort_spark("sp-e", 2, "task", "open"),
            mk_sort_spark("sp-a", 0, "bug", "in_progress"),
            mk_sort_spark("sp-b", 0, "epic", "open"),
            mk_sort_spark("sp-d", 1, "feature", "blocked"),
            mk_sort_spark("sp-c", 0, "bug", "in_progress"),
            mk_sort_spark("sp-f", 2, "task", "open"),
            mk_sort_spark("sp-g", 0, "bug", "blocked"),
            mk_sort_spark("sp-h", 1, "feature", "open"),
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
            vec![
                "sp-b", "sp-a", "sp-c", "sp-g", "sp-d", "sp-h", "sp-e", "sp-f"
            ]
        );
    }

    #[test]
    fn default_sort_is_deterministic_across_calls() {
        let sparks = vec![
            mk_sort_spark("sp-2", 1, "task", "open"),
            mk_sort_spark("sp-1", 1, "task", "open"),
            mk_sort_spark("sp-3", 0, "bug", "blocked"),
        ];
        let a: Vec<&str> = default_sort(&sparks)
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        let b: Vec<&str> = default_sort(&sparks)
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(a, b);
    }

    #[test]
    fn default_sort_type_order_is_fixed() {
        let sparks = vec![
            mk_sort_spark("a", 0, "milestone", "open"),
            mk_sort_spark("b", 0, "chore", "open"),
            mk_sort_spark("c", 0, "spike", "open"),
            mk_sort_spark("d", 0, "task", "open"),
            mk_sort_spark("e", 0, "feature", "open"),
            mk_sort_spark("f", 0, "bug", "open"),
            mk_sort_spark("g", 0, "epic", "open"),
        ];
        let ids: Vec<&str> = default_sort(&sparks)
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["g", "f", "e", "d", "c", "b", "a"]);
    }

    #[test]
    fn default_sort_status_order_is_fixed() {
        let sparks = vec![
            mk_sort_spark("a", 0, "task", "closed"),
            mk_sort_spark("b", 0, "task", "completed"),
            mk_sort_spark("c", 0, "task", "deferred"),
            mk_sort_spark("d", 0, "task", "open"),
            mk_sort_spark("e", 0, "task", "blocked"),
            mk_sort_spark("f", 0, "task", "in_progress"),
        ];
        let ids: Vec<&str> = default_sort(&sparks)
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["f", "e", "d", "c", "b", "a"]);
    }

    #[test]
    fn default_sort_unknown_type_and_status_sink_to_end() {
        let sparks = vec![
            mk_sort_spark("a", 0, "zzz", "open"),
            mk_sort_spark("b", 0, "task", "open"),
            mk_sort_spark("c", 0, "task", "zzz"),
        ];
        let ids: Vec<&str> = default_sort(&sparks)
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        // Known type before unknown; within task, known status before unknown.
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn validate_requires_title() {
        let mut f = CreateForm::default();
        f.spark_type = "epic".into();
        assert!(f.validate().is_err());
        f.title = "hello".into();
        assert!(f.validate().is_ok());
    }

    #[test]
    fn validate_rejects_orphan_non_epic() {
        // Mirrors the data-layer invariant: non-epic without parent =>
        // error. Epic with no parent is fine.
        let mut f = CreateForm {
            title: "task".into(),
            spark_type: "task".into(),
            priority: 2,
            parent_epic_id: None,
            error: None,
            visible: true,
        };
        assert!(f.validate().is_err());
        assert!(!f.is_valid());
        f.parent_epic_id = Some("ryve-epic".into());
        assert!(f.validate().is_ok());
        assert!(f.is_valid());
    }

    #[test]
    fn validate_allows_top_level_epic() {
        let f = CreateForm {
            title: "epic".into(),
            spark_type: "epic".into(),
            priority: 1,
            parent_epic_id: None,
            error: None,
            visible: true,
        };
        assert!(f.is_valid());
    }

    #[test]
    fn open_with_default_parent_seeds_and_shows() {
        let mut f = CreateForm::default();
        f.open_with_default_parent(Some("ryve-epic".into()));
        assert!(f.visible);
        assert_eq!(f.parent_epic_id.as_deref(), Some("ryve-epic"));
        assert_eq!(f.spark_type, "task");
        assert_eq!(f.priority, 2);
    }

    #[test]
    fn open_with_no_default_parent_leaves_empty() {
        let mut f = CreateForm::default();
        f.open_with_default_parent(None);
        assert!(f.visible);
        assert!(f.parent_epic_id.is_none());
    }

    #[test]
    fn default_parent_none_when_nothing_focused() {
        let sparks = vec![mk_spark("a", "epic", None)];
        assert!(resolve_default_parent_epic(&sparks, None).is_none());
    }

    #[test]
    fn default_parent_is_focused_epic_itself() {
        let sparks = vec![mk_spark("epic-1", "epic", None)];
        let out = resolve_default_parent_epic(&sparks, Some("epic-1"));
        assert_eq!(out.as_deref(), Some("epic-1"));
    }

    #[test]
    fn default_parent_is_focused_tasks_parent_epic() {
        let sparks = vec![
            mk_spark("epic-1", "epic", None),
            mk_spark("task-1", "task", Some("epic-1")),
        ];
        let out = resolve_default_parent_epic(&sparks, Some("task-1"));
        assert_eq!(out.as_deref(), Some("epic-1"));
    }

    #[test]
    fn default_parent_walks_nested_chain_to_epic() {
        // task -> task -> epic : the closest epic ancestor is returned.
        let sparks = vec![
            mk_spark("epic-1", "epic", None),
            mk_spark("mid", "task", Some("epic-1")),
            mk_spark("leaf", "task", Some("mid")),
        ];
        let out = resolve_default_parent_epic(&sparks, Some("leaf"));
        assert_eq!(out.as_deref(), Some("epic-1"));
    }

    #[test]
    fn default_parent_none_when_focused_id_missing() {
        let sparks = vec![mk_spark("epic-1", "epic", None)];
        assert!(resolve_default_parent_epic(&sparks, Some("ghost")).is_none());
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

    fn mk_spark(id: &str, spark_type: &str, parent: Option<&str>) -> Spark {
        Spark {
            id: id.to_string(),
            title: id.to_string(),
            description: String::new(),
            status: "open".to_string(),
            priority: 2,
            spark_type: spark_type.to_string(),
            assignee: None,
            owner: None,
            parent_id: parent.map(|s| s.to_string()),
            workshop_id: "ws".to_string(),
            estimated_minutes: None,
            github_issue_number: None,
            github_repo: None,
            metadata: "{}".to_string(),
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
    fn group_by_epic_mixed_tree_places_children_under_their_parent() {
        let sparks = vec![
            mk_spark("e1", "epic", None),
            mk_spark("e2", "epic", None),
            mk_spark("t1", "task", Some("e1")),
            mk_spark("t2", "task", Some("e2")),
            mk_spark("t3", "task", Some("e1")),
        ];

        let groups = group_by_epic(&sparks);
        assert_eq!(groups.len(), 2);

        assert_eq!(groups[0].epic.id, "e1");
        let e1_children: Vec<&str> = groups[0].children.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(e1_children, vec!["t1", "t3"]);

        assert_eq!(groups[1].epic.id, "e2");
        let e2_children: Vec<&str> = groups[1].children.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(e2_children, vec!["t2"]);
    }

    #[test]
    fn group_by_epic_empty_epic_still_renders_as_empty_group() {
        let sparks = vec![
            mk_spark("e1", "epic", None),
            mk_spark("e_empty", "epic", None),
            mk_spark("t1", "task", Some("e1")),
        ];
        let groups = group_by_epic(&sparks);
        assert_eq!(groups.len(), 2);
        let empty = groups.iter().find(|g| g.epic.id == "e_empty").unwrap();
        assert!(empty.children.is_empty());
    }

    #[test]
    fn group_by_epic_nested_epic_is_both_child_and_its_own_group() {
        let sparks = vec![
            mk_spark("outer", "epic", None),
            mk_spark("inner", "epic", Some("outer")),
            mk_spark("leaf", "task", Some("inner")),
        ];
        let groups = group_by_epic(&sparks);
        assert_eq!(groups.len(), 2);

        let outer = &groups[0];
        assert_eq!(outer.epic.id, "outer");
        let outer_children: Vec<&str> = outer.children.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(outer_children, vec!["inner"]);

        let inner = &groups[1];
        assert_eq!(inner.epic.id, "inner");
        let inner_children: Vec<&str> = inner.children.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(inner_children, vec!["leaf"]);
    }

    #[test]
    fn group_by_epic_preserves_input_order_for_children() {
        // The renderer relies on group_by_epic pushing children in the
        // exact order they appear in the input vec, so the upstream
        // default sort (priority, type, status, id) flows through.
        let sparks = vec![
            mk_spark("e1", "epic", None),
            mk_spark("t_b", "task", Some("e1")),
            mk_spark("t_a", "task", Some("e1")),
            mk_spark("t_c", "task", Some("e1")),
        ];
        let groups = group_by_epic(&sparks);
        let ids: Vec<&str> = groups[0].children.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["t_b", "t_a", "t_c"]);
    }

    #[test]
    fn group_by_epic_drops_orphan_non_epic_sparks() {
        // A non-epic with no parent (or a missing parent) must never
        // appear at top level. Upstream is expected to assign a parent,
        // but we defensively drop rather than render outside a group.
        let sparks = vec![
            mk_spark("e1", "epic", None),
            mk_spark("orphan", "task", None),
            mk_spark("stray", "task", Some("nope")),
        ];
        let groups = group_by_epic(&sparks);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].children.is_empty());
    }

    #[test]
    fn collapsed_epics_toggle_is_symmetric() {
        let mut c = HashSet::<String>::new();
        assert!(!c.contains("e1"));
        c.insert("e1".to_string());
        assert!(c.contains("e1"));
        c.remove("e1");
        assert!(!c.contains("e1"));
    }

    #[test]
    fn refresh_glyph_swaps_while_refetching() {
        // Spark ryve-7805b38b: the Workgraph refresh button must surface
        // a visible in-flight indicator so clicks feel responsive. The
        // two glyphs must differ so a user can see the state change.
        let idle = refresh_button_glyph(false);
        let busy = refresh_button_glyph(true);
        assert_ne!(idle, busy);
        assert_eq!(idle, "\u{21BB}");
        assert_eq!(busy, "\u{2026}");
    }

    #[test]
    fn status_symbol_distinguishes_states() {
        assert_ne!(status_symbol("open"), status_symbol("in_progress"));
        assert_ne!(status_symbol("blocked"), status_symbol("deferred"));
        assert_ne!(status_symbol("closed"), status_symbol("open"));
    }

    // ── SparksFilter tests ───────────────────────────────

    #[test]
    fn filter_default_shows_all_except_closed() {
        let f = SparksFilter::default();
        assert!(f.matches("open"));
        assert!(f.matches("in_progress"));
        assert!(f.matches("blocked"));
        assert!(f.matches("deferred"));
        assert!(f.matches("completed"));
        assert!(!f.matches("closed"));
    }

    #[test]
    fn filter_toggle_adds_and_removes_status() {
        let mut f = SparksFilter::default();
        assert!(f.toggle_status("blocked"));
        assert!(f.status.contains("blocked"));
        assert!(!f.toggle_status("blocked"));
        assert!(!f.status.contains("blocked"));
    }

    #[test]
    fn filter_with_selected_status_only_shows_selected() {
        let mut f = SparksFilter::default();
        f.toggle_status("in_progress");
        assert!(f.matches("in_progress"));
        assert!(!f.matches("open"));
        assert!(!f.matches("blocked"));
        assert!(!f.matches("deferred"));
        assert!(!f.matches("closed"));
    }

    #[test]
    fn filter_show_closed_includes_closed_sparks() {
        let mut f = SparksFilter::default();
        assert!(!f.matches("closed"));
        f.show_closed = true;
        assert!(f.matches("closed"));
    }

    #[test]
    fn filter_empty_status_set_is_show_all() {
        let f = SparksFilter::default();
        assert!(f.status.is_empty());
        assert!(f.matches("open"));
        assert!(f.matches("in_progress"));
        assert!(f.matches("blocked"));
        assert!(f.matches("deferred"));
    }

    #[test]
    fn filter_multiple_statuses_selected() {
        let mut f = SparksFilter::default();
        f.toggle_status("open");
        f.toggle_status("blocked");
        assert!(f.matches("open"));
        assert!(f.matches("blocked"));
        assert!(!f.matches("in_progress"));
        assert!(!f.matches("deferred"));
    }

    #[test]
    fn filter_closed_obeys_own_toggle_even_when_status_selected() {
        let mut f = SparksFilter::default();
        f.toggle_status("in_progress");
        assert!(!f.matches("closed"));
        f.show_closed = true;
        assert!(f.matches("closed"));
    }

    #[test]
    fn filter_pill_state_mirrors_sparks_filter() {
        // Invariant: pill state is a direct mirror of SparksFilter.
        let mut f = SparksFilter::default();
        f.toggle_status("blocked");
        assert!(f.status.contains("blocked"));
        assert_eq!(f.status.len(), 1);
        f.toggle_status("blocked");
        assert!(f.status.is_empty());
    }

    #[test]
    fn filter_statuses_cover_expected_values() {
        let keys: Vec<&str> = FILTER_STATUSES.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            vec!["open", "in_progress", "blocked", "deferred", "completed"]
        );
    }

    #[test]
    fn status_color_returns_distinct_colors_for_key_statuses() {
        let pal = crate::style::Palette::dark();
        assert_ne!(
            status_color("open", &pal),
            status_color("in_progress", &pal)
        );
        assert_ne!(
            status_color("blocked", &pal),
            status_color("deferred", &pal)
        );
        assert_ne!(
            status_color("in_progress", &pal),
            status_color("blocked", &pal)
        );
    }
}
