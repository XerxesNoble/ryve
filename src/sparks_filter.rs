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
//!
//! Both `SparksFilter` and `SortMode` derive `Serialize`/`Deserialize` so
//! the filter state can be persisted in `.ryve/ui_state.json` per workshop.
//! Spark ryve-d6916c7e (struct), ryve-27e33825 (persistence).

use std::collections::HashSet;

use data::sparks::types::Spark;
use serde::{Deserialize, Serialize};

// ── SortMode ──────────────────────────────────────────

/// How the filtered spark list should be ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

// ── SparksFilter ──────────────────────────────────────

/// Complete filter + sort state for the sparks panel.
///
/// An empty `HashSet` on any dimension means "allow all values for that
/// dimension".  `show_closed = false` (the default) hides sparks whose
/// `status == "closed"`.
///
/// `Serialize`/`Deserialize` so the filter can be persisted in
/// `.ryve/ui_state.json`. Spark ryve-27e33825.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SparksFilter {
    #[serde(default)]
    pub status: HashSet<String>,
    #[serde(default)]
    pub spark_type: HashSet<String>,
    #[serde(default)]
    pub priority: HashSet<i32>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub sort_mode: SortMode,
    #[serde(default)]
    pub show_closed: bool,
}

// ── Persistence conversion (spark ryve-27e33825) ──────

impl SparksFilter {
    /// Convert to the data-crate `SparksFilterState` for persistence.
    pub fn to_persisted(&self) -> data::ryve_dir::SparksFilterState {
        data::ryve_dir::SparksFilterState {
            status: self.status.clone(),
            spark_type: self.spark_type.clone(),
            priority: self.priority.clone(),
            assignee: self.assignee.clone(),
            search: self.search.clone(),
            sort_mode: match self.sort_mode {
                SortMode::Default => "default".to_string(),
                SortMode::PriorityOnly => "priority_only".to_string(),
                SortMode::RecentlyUpdated => "recently_updated".to_string(),
                SortMode::TypeFirst => "type_first".to_string(),
            },
            show_closed: self.show_closed,
        }
    }

    /// Restore from a persisted `SparksFilterState`.
    pub fn from_persisted(p: &data::ryve_dir::SparksFilterState) -> Self {
        Self {
            status: p.status.clone(),
            spark_type: p.spark_type.clone(),
            priority: p.priority.clone(),
            assignee: p.assignee.clone(),
            search: p.search.clone(),
            sort_mode: match p.sort_mode.as_str() {
                "priority_only" => SortMode::PriorityOnly,
                "recently_updated" => SortMode::RecentlyUpdated,
                "type_first" => SortMode::TypeFirst,
                _ => SortMode::Default,
            },
            show_closed: p.show_closed,
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
            // show_closed gate
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

fn sort_sparks(sparks: &mut [&Spark], mode: SortMode) {
    match mode {
        SortMode::Default => {
            sparks.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| a.spark_type.cmp(&b.spark_type))
                    .then_with(|| a.status.cmp(&b.status))
            });
        }
        SortMode::PriorityOnly => {
            sparks.sort_by(|a, b| a.priority.cmp(&b.priority));
        }
        SortMode::RecentlyUpdated => {
            sparks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        }
        SortMode::TypeFirst => {
            sparks.sort_by(|a, b| {
                a.spark_type
                    .cmp(&b.spark_type)
                    .then_with(|| a.priority.cmp(&b.priority))
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
        assert_eq!(result[0].id, "b");
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

    // ── Serialization round-trip (spark ryve-27e33825) ────

    #[test]
    fn sparks_filter_serialize_roundtrip() {
        let filter = SparksFilter {
            status: HashSet::from(["open".to_string()]),
            priority: HashSet::from([0, 1]),
            show_closed: true,
            sort_mode: SortMode::RecentlyUpdated,
            search: "auth".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&filter).unwrap();
        let restored: SparksFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, restored);
    }

    #[test]
    fn sparks_filter_deserialize_missing_fields_uses_defaults() {
        // An empty JSON object should produce the default filter —
        // forward compatibility when new fields are added.
        let filter: SparksFilter = serde_json::from_str("{}").unwrap();
        assert_eq!(filter, SparksFilter::default());
    }

    #[test]
    fn to_persisted_and_from_persisted_roundtrip() {
        let filter = SparksFilter {
            status: HashSet::from(["open".to_string(), "blocked".to_string()]),
            priority: HashSet::from([0, 1]),
            show_closed: true,
            sort_mode: SortMode::RecentlyUpdated,
            search: "auth".to_string(),
            assignee: Some("alice".to_string()),
            ..Default::default()
        };
        let persisted = filter.to_persisted();
        let restored = SparksFilter::from_persisted(&persisted);
        assert_eq!(filter, restored);
    }

    #[test]
    fn from_persisted_unknown_sort_mode_defaults() {
        let persisted = data::ryve_dir::SparksFilterState {
            sort_mode: "unknown_future_mode".to_string(),
            ..Default::default()
        };
        let restored = SparksFilter::from_persisted(&persisted);
        assert_eq!(restored.sort_mode, SortMode::Default);
    }
}
