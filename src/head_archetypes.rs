// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Head archetype registry.
//!
//! A **Head archetype** is a specialization of the generic Head role
//! (see [`crate::agent_prompts::compose_head_prompt`]) with a purpose-built
//! prompt template. The generic Head prompt is composed in Rust code and
//! only parameterized by `epic_id` / `epic_title`. Archetypes instead load
//! a checked-in markdown template from `src/head_templates/` and render it
//! by substituting placeholders at spawn time.
//!
//! This keeps specialized Head specs in plain files that can be reviewed
//! and edited without rebuilding the binary. Ryve embeds the template at
//! compile time via [`include_str!`] so the shipping binary always has a
//! known-good copy, and so tests can run without touching the filesystem.
//!
//! The first archetype is **PerfHead** — the workflow that shipped the P1
//! perf remediation epic by hand. Its template is the headless spec a
//! performance-remediation Head needs: read epic, decompose into perf
//! sparks, spawn Hands in a Crew, poll progress, reassign on stall, spawn
//! merger.

/// A registered Head archetype: a named specialization of the Head role
/// backed by a checked-in prompt template.
#[derive(Debug, Clone, Copy)]
pub struct HeadArchetypeTemplate {
    /// Short, stable identifier used on the CLI (e.g. `perf`).
    pub id: &'static str,
    /// Human-readable name (e.g. `PerfHead`).
    pub display_name: &'static str,
    /// One-line summary shown in `ryve head archetype list`.
    pub description: &'static str,
    /// Workspace-relative path of the template file the archetype points
    /// at. Kept for documentation / tooling; the actual bytes are embedded
    /// in [`HeadArchetypeTemplate::template`] via `include_str!`.
    pub template_path: &'static str,
    /// The prompt template, embedded at compile time. Contains
    /// `{{epic_id}}` placeholders that [`HeadArchetypeTemplate::render`] will
    /// substitute.
    pub template: &'static str,
}

impl HeadArchetypeTemplate {
    /// Render the archetype's prompt template for a concrete epic.
    ///
    /// Replaces every occurrence of `{{epic_id}}` with the supplied id.
    /// The template is expected to carry the `{{epic_id}}` placeholder at
    /// least once; the test suite guarantees this for every registered
    /// archetype so a typo in the template cannot ship silently.
    pub fn render(&self, epic_id: &str) -> String {
        self.template.replace("{{epic_id}}", epic_id)
    }
}

/// PerfHead: performance-remediation Head archetype.
pub const PERF_HEAD: HeadArchetypeTemplate = HeadArchetypeTemplate {
    id: "perf",
    display_name: "PerfHead",
    description: "Performance remediation: decompose a perf epic into \
                  measurable optimization sparks, spawn a Crew of Hands, \
                  reassign on stall, merge via a Merger Hand.",
    template_path: "src/head_templates/perf_head.md",
    template: include_str!("head_templates/perf_head.md"),
};

/// All registered Head archetypes. Add new entries here and they become
/// visible to `ryve head archetype list` and to any selection UI.
pub const ARCHETYPES: &[&HeadArchetypeTemplate] = &[&PERF_HEAD];

/// Look up an archetype by its short id.
pub fn find(id: &str) -> Option<&'static HeadArchetypeTemplate> {
    ARCHETYPES.iter().copied().find(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfhead_is_registered_and_findable() {
        assert!(find("perf").is_some());
        let a = find("perf").unwrap();
        assert_eq!(a.id, "perf");
        assert_eq!(a.display_name, "PerfHead");
        assert_eq!(a.template_path, "src/head_templates/perf_head.md");
        assert!(!a.template.is_empty());
    }

    #[test]
    fn find_returns_none_for_unknown_archetype() {
        assert!(find("does-not-exist").is_none());
    }

    #[test]
    fn every_archetype_template_carries_the_epic_id_placeholder() {
        for a in ARCHETYPES {
            assert!(
                a.template.contains("{{epic_id}}"),
                "archetype `{}` template is missing the {{{{epic_id}}}} placeholder",
                a.id
            );
        }
    }

    #[test]
    fn perfhead_template_covers_required_workflow_stages() {
        let tpl = PERF_HEAD.template;
        // read epic
        assert!(tpl.contains("ryve spark show {{epic_id}}"));
        // decompose
        assert!(tpl.contains("ryve spark create"));
        assert!(tpl.contains("parent_child"));
        // spawn Hands in a Crew
        assert!(tpl.contains("ryve crew create"));
        assert!(tpl.contains("ryve hand spawn"));
        // poll progress (no busy wait)
        assert!(tpl.contains("ryve crew show"));
        assert!(tpl.to_lowercase().contains("poll"));
        // reassign on stall
        assert!(tpl.contains("ryve assign release"));
        assert!(tpl.to_lowercase().contains("stall") || tpl.to_lowercase().contains("stalled"));
        // spawn merger
        assert!(tpl.contains("--role merger"));
    }

    /// Dry-run smoke test: render PerfHead against a fake epic id and
    /// verify the id is substituted everywhere the placeholder appeared,
    /// and that no placeholders remain. Satisfies the "dry-run / smoke
    /// test launches PerfHead against a fake epic and verifies the
    /// prompt is rendered with the epic id substituted" acceptance
    /// criterion of spark `ryve-53bb0bac` [sp-fbf2a519].
    #[test]
    fn render_substitutes_fake_epic_id_everywhere() {
        let fake = "ep-fake-smoke-123";
        let rendered = PERF_HEAD.render(fake);

        // No leftover placeholders.
        assert!(
            !rendered.contains("{{epic_id}}"),
            "render left an unsubstituted {{{{epic_id}}}} placeholder"
        );

        // The fake id appears at least as many times as the placeholder
        // did in the source template.
        let placeholder_count = PERF_HEAD.template.matches("{{epic_id}}").count();
        assert!(placeholder_count > 0);
        assert_eq!(
            rendered.matches(fake).count(),
            placeholder_count,
            "every {{{{epic_id}}}} placeholder should be replaced with the epic id"
        );

        // Rendered prompt still opens with the identity line and hard rules.
        assert!(rendered.starts_with("You are **PerfHead**"));
        assert!(rendered.contains("HARD RULES"));
    }
}
