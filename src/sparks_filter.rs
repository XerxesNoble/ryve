// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Filter and sort state for the sparks panel.
//!
//! `SparksFilter` holds every dimension the user can constrain — status,
//! type, priority, assignee, free-text search, sort order, and the
//! show-closed toggle.  Empty sets mean "no constraint on that dimension"
//! (allow all), **not** "match nothing".
//!
//! `apply_filter` is a pure function: it borrows a filter and a slice of
//! sparks and returns the filtered + sorted subset.

use std::collections::HashSet;

use data::sparks::types::Spark;

// ── SortMode ──────────────────────────────────────────

/// How the filtered spark list should be ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    /// Priority (ascending) → type → status.
    #[default]
    Default,
    /// Priority ascending only.
    PriorityOnly,
    /// Most-recently updated first.
    RecentlyUpdated,
    /// Type alphabetical → priority ascending.
    TypeFirst,
}

impl SortMode {
    /// Human-readable label shown on the dropdown button.
    pub fn display_name(self) -> &'static str {
        match self {
            SortMode::Default => "Default",
            SortMode::PriorityOnly => "Priority only",
            SortMode::RecentlyUpdated => "Recently updated",
            SortMode::TypeFirst => "By type",
        }
    }

    /// All modes in display order.
    pub const ALL: &[SortMode] = &[
        SortMode::Default,
        SortMode::PriorityOnly,
        SortMode::RecentlyUpdated,
        SortMode::TypeFirst,
    ];

    /// Stable string key used for JSON persistence.
    pub fn to_persist_key(self) -> &'static str {
        match self {
            SortMode::Default => "default",
            SortMode::PriorityOnly => "priority_only",
            SortMode::RecentlyUpdated => "recently_updated",
            SortMode::TypeFirst => "type_first",
        }
    }

    /// Parse from a persisted string key; unknown values fall back to
    /// `Default`.
    pub fn from_persist_key(s: &str) -> Self {
        match s {
            "default" => SortMode::Default,
            "priority_only" => SortMode::PriorityOnly,
            "recently_updated" => SortMode::RecentlyUpdated,
            "type_first" => SortMode::TypeFirst,
            _ => SortMode::Default,
        }
    }
}

// ── SparksFilter ──────────────────────────────────────

/// Complete filter + sort state for the sparks panel.
///
/// An empty `HashSet` on any dimension means "allow all values for that
/// dimension".  `show_closed = false` (the default) hides sparks whose
/// `status == "closed"`.  Completed sparks remain visible regardless of
/// the toggle — they are a distinct terminal state.
pub struct SparksFilter {
    pub status: HashSet<String>,
    pub spark_type: HashSet<String>,
    pub priority: HashSet<i32>,
    pub assignee: Option<String>,
    pub search: String,
    pub sort_mode: SortMode,
    pub show_closed: bool,
}

impl Default for SparksFilter {
    fn default() -> Self {
        Self {
            status: HashSet::new(),
            spark_type: HashSet::new(),
            priority: HashSet::new(),
            assignee: None,
            search: String::new(),
            sort_mode: SortMode::Default,
            show_closed: false,
        }
    }
}

// ── apply_filter ──────────────────────────────────────

/// Return the subset of `sparks` that match `filter`, sorted according to
/// `filter.sort_mode`.
pub fn apply_filter<'a>(filter: &SparksFilter, sparks: &'a [Spark]) -> Vec<&'a Spark> {
    let search_lower = filter.search.to_lowercase();

    let mut out: Vec<&Spark> = sparks
        .iter()
        .filter(|s| {
            // show_closed gate — only hides `closed`; `completed` is a
            // distinct terminal state and remains visible.
            if !filter.show_closed && s.status == "closed" {
                return false;
            }

            // status set
            if !filter.status.is_empty() && !filter.status.contains(&s.status) {
                return false;
            }

            // type set
            if !filter.spark_type.is_empty() && !filter.spark_type.contains(&s.spark_type) {
                return false;
            }

            // priority set
            if !filter.priority.is_empty() && !filter.priority.contains(&s.priority) {
                return false;
            }

            // assignee
            if let Some(ref wanted) = filter.assignee {
                match &s.assignee {
                    Some(a) if a == wanted => {}
                    _ => return false,
                }
            }

            // free-text search (case-insensitive on title + description)
            if !search_lower.is_empty() {
                let title_match = s.title.to_lowercase().contains(&search_lower);
                let desc_match = s.description.to_lowercase().contains(&search_lower);
                if !title_match && !desc_match {
                    return false;
                }
            }

            true
        })
        .collect();

    sort_sparks(&mut out, filter.sort_mode);
    out
}

const SPARK_TYPE_ORDER: &[&str] = &[
    "epic",
    "bug",
    "feature",
    "task",
    "spike",
    "chore",
    "milestone",
];

const STATUS_ORDER: &[&str] = &[
    "in_progress",
    "blocked",
    "open",
    "deferred",
    "completed",
    "closed",
];

fn type_rank(ty: &str) -> usize {
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

fn sort_sparks(sparks: &mut [&Spark], mode: SortMode) {
    match mode {
        SortMode::Default => {
            sparks.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| type_rank(&a.spark_type).cmp(&type_rank(&b.spark_type)))
                    .then_with(|| status_rank(&a.status).cmp(&status_rank(&b.status)))
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
        SortMode::PriorityOnly => {
            sparks.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
        }
        SortMode::RecentlyUpdated => {
            sparks.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
        SortMode::TypeFirst => {
            sparks.sort_by(|a, b| {
                type_rank(&a.spark_type)
                    .cmp(&type_rank(&b.spark_type))
                    .then_with(|| a.priority.cmp(&b.priority))
                    .then_with(|| status_rank(&a.status).cmp(&status_rank(&b.status)))
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spark(id: &str, title: &str, status: &str, priority: i32, spark_type: &str) -> Spark {
        Spark {
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            status: status.to_string(),
            priority,
            spark_type: spark_type.to_string(),
            assignee: None,
            owner: None,
            parent_id: None,
            workshop_id: "ws-1".to_string(),
            estimated_minutes: None,
            github_issue_number: None,
            github_repo: None,
            metadata: "{}".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            closed_at: None,
            closed_reason: None,
            due_at: None,
            defer_until: None,
            risk_level: None,
            scope_boundary: None,
        }
    }

    #[test]
    fn default_filter_hides_closed() {
        let sparks = vec![
            make_spark("a", "Open task", "open", 1, "task"),
            make_spark("b", "Closed task", "closed", 1, "task"),
        ];
        let filter = SparksFilter::default();
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn show_closed_reveals_them() {
        let sparks = vec![
            make_spark("a", "Open task", "open", 1, "task"),
            make_spark("b", "Closed task", "closed", 1, "task"),
        ];
        let filter = SparksFilter {
            show_closed: true,
            ..Default::default()
        };
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn status_filter_narrows_correctly() {
        let sparks = vec![
            make_spark("a", "Open", "open", 1, "task"),
            make_spark("b", "In progress", "in_progress", 1, "task"),
            make_spark("c", "Blocked", "blocked", 2, "task"),
        ];
        let filter = SparksFilter {
            status: HashSet::from(["open".to_string()]),
            ..Default::default()
        };
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn search_matches_case_insensitively_on_title() {
        let sparks = vec![
            make_spark("a", "Fix Authentication Bug", "open", 1, "bug"),
            make_spark("b", "Add logging", "open", 2, "task"),
        ];
        let filter = SparksFilter {
            search: "auth".to_string(),
            ..Default::default()
        };
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn search_matches_case_insensitively_on_description() {
        let mut spark = make_spark("a", "Some task", "open", 1, "task");
        spark.description = "Improve the Authentication flow".to_string();
        let sparks = vec![spark, make_spark("b", "Other task", "open", 2, "task")];
        let filter = SparksFilter {
            search: "auth".to_string(),
            ..Default::default()
        };
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn empty_filter_sets_allow_all_non_closed() {
        let sparks = vec![
            make_spark("a", "A", "open", 0, "bug"),
            make_spark("b", "B", "in_progress", 1, "task"),
            make_spark("c", "C", "blocked", 2, "feature"),
        ];
        let filter = SparksFilter::default();
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn default_sort_is_priority_type_status() {
        let sparks = vec![
            make_spark("a", "A", "open", 2, "task"),
            make_spark("b", "B", "open", 1, "bug"),
            make_spark("c", "C", "open", 1, "task"),
        ];
        let filter = SparksFilter::default();
        let result = apply_filter(&filter, &sparks);
        // P1 bug, P1 task, P2 task
        assert_eq!(result[0].id, "b"); // P1, bug
        assert_eq!(result[1].id, "c"); // P1, task
        assert_eq!(result[2].id, "a"); // P2, task
    }

    #[test]
    fn recently_updated_sort() {
        let mut s1 = make_spark("a", "Old", "open", 1, "task");
        s1.updated_at = "2026-01-01T00:00:00Z".to_string();
        let mut s2 = make_spark("b", "New", "open", 1, "task");
        s2.updated_at = "2026-04-01T00:00:00Z".to_string();
        let sparks = vec![s1, s2];
        let filter = SparksFilter {
            sort_mode: SortMode::RecentlyUpdated,
            ..Default::default()
        };
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result[0].id, "b"); // newer first
        assert_eq!(result[1].id, "a");
    }

    #[test]
    fn assignee_filter() {
        let mut s1 = make_spark("a", "Mine", "open", 1, "task");
        s1.assignee = Some("alice".to_string());
        let mut s2 = make_spark("b", "Theirs", "open", 1, "task");
        s2.assignee = Some("bob".to_string());
        let s3 = make_spark("c", "Unassigned", "open", 1, "task");
        let sparks = vec![s1, s2, s3];
        let filter = SparksFilter {
            assignee: Some("alice".to_string()),
            ..Default::default()
        };
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn priority_filter() {
        let sparks = vec![
            make_spark("a", "P0", "open", 0, "task"),
            make_spark("b", "P1", "open", 1, "task"),
            make_spark("c", "P2", "open", 2, "task"),
        ];
        let filter = SparksFilter {
            priority: HashSet::from([0, 2]),
            ..Default::default()
        };
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result.len(), 2);
        let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"c"));
    }

    #[test]
    fn type_filter() {
        let sparks = vec![
            make_spark("a", "Bug", "open", 1, "bug"),
            make_spark("b", "Task", "open", 1, "task"),
            make_spark("c", "Epic", "open", 1, "epic"),
        ];
        let filter = SparksFilter {
            spark_type: HashSet::from(["bug".to_string(), "epic".to_string()]),
            ..Default::default()
        };
        let result = apply_filter(&filter, &sparks);
        assert_eq!(result.len(), 2);
        let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"c"));
    }
}
