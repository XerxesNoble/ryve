// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Bench panel — tabbed workspace for terminal sessions and coding agents.

use std::collections::HashMap;
use std::path::PathBuf;

use data::ryve_dir::AgentDef;
use iced::widget::{
    Space, button, column, container, mouse_area, row, scrollable, text, text_input, tooltip,
};
use iced::{Color, Element, Length, Theme};

use crate::coding_agents::CodingAgent;
use crate::style::{self, FONT_BODY, FONT_ICON, FONT_ICON_SM, FONT_LABEL, FONT_SMALL, Palette};

/// A tab in the bench — either a plain terminal, coding agent, or file viewer.
#[derive(Debug, Clone)]
pub struct Tab {
    pub id: u64,
    pub title: String,
    pub kind: TabKind,
    /// When true the tab cannot be closed by the user — the close button is
    /// hidden and [`Message::CloseTab`] is a no-op. Used for the Atlas
    /// Director tab (spark ryve-59983890).
    pub pinned: bool,
}

#[derive(Debug, Clone)]
pub enum TabKind {
    Terminal,
    CodingAgent(CodingAgent),
    FileViewer(PathBuf),
    /// Multi-Hand coordination dashboard. Singleton per workshop —
    /// `Workshop::open_home_tab` enforces "at most one" so the dropdown
    /// entry can be pressed repeatedly without spawning duplicates.
    Home,
    /// Read-only spy view tailing the log file of a CLI-spawned background
    /// Hand. The string carries the agent_session id so the parent app can
    /// drive periodic reloads of the right tab. Spark ryve-8c14734a.
    LogTail {
        session_id: String,
        log_path: PathBuf,
    },
}

/// Per-terminal-tab Cmd+F search state. Lives in [`BenchState`] keyed
/// by tab id so each terminal remembers its own query while the user
/// switches between tabs. Match positions are stored as opaque indices
/// into the live result list returned by `Terminal::search`; we
/// re-run the search whenever the query changes so it stays in sync
/// with the terminal's scrollback.
#[derive(Debug, Default, Clone)]
pub struct TerminalSearchState {
    pub query: String,
    /// Number of matches the last search produced. We don't keep the
    /// `Match` values themselves because they reference grid points
    /// that may shift as the terminal scrolls — we just need the count
    /// for the "x / N" indicator.
    pub match_count: usize,
    /// 0-based index of the currently focused match within
    /// `match_count`. None when there are no matches.
    pub current_match: Option<usize>,
}

/// Stable widget id for the terminal search input — only one is
/// visible at a time so a single id is fine.
pub const TERMINAL_SEARCH_INPUT_ID: &str = "bench-terminal-search-input";

/// State for the bench panel.
pub struct BenchState {
    pub tabs: Vec<Tab>,
    pub active_tab: Option<u64>,
    pub dropdown_open: bool,
    /// Cmd+F search overlay state per terminal tab. A tab is in
    /// "search open" mode iff it has an entry here. Closing the search
    /// removes the entry so the overlay disappears entirely.
    pub terminal_search: HashMap<u64, TerminalSearchState>,
}

#[derive(Debug, Clone)]
pub enum Message {
    SelectTab(u64),
    CloseTab(u64),
    ToggleDropdown,
    /// Close the "+" dropdown without taking any other action. Emitted by
    /// the backdrop mouse_area so a click anywhere outside the menu
    /// dismisses it (sp-ux0022).
    CloseDropdown,
    /// Swallowed click on the dropdown container itself — prevents the
    /// backdrop from closing the menu when the user clicks an empty
    /// gap inside it (sp-ux0022).
    NoOp,
    NewTerminal,
    /// Open (or focus) the Home / multi-Hand coordination dashboard tab.
    /// Singleton per workshop.
    OpenHome,
    /// Spawn **Atlas** — Ryve's primary user-facing Director agent. This is
    /// the **default entry point** for top-level user requests
    /// (spark ryve-acdb248a). Atlas talks to the user, classifies their
    /// intent, and delegates to a Head or a Hand instead of executing code
    /// itself. New Hand / New Head / New Terminal remain as documented
    /// bypass paths for advanced flows that want to skip routing through
    /// Atlas; see `docs/ATLAS.md`.
    NewAtlas,
    /// BYPASS: open the spark+agent picker for spawning a regular Hand on
    /// a spark directly, without going through Atlas. For advanced users
    /// who already know which spark they want to claim.
    NewHand,
    /// BYPASS: open the agent picker for spawning a Head directly. The
    /// Head will mint its own Crew. Skips the Atlas routing layer.
    NewHead,
    /// Legacy: directly spawn a coding agent of the given type. Still emitted
    /// from the Hand picker once the user has chosen both the spark and the
    /// agent.
    NewCodingAgent(CodingAgent),
    NewCustomAgent(usize),
    TerminalEvent(iced_term::Event),
    /// Open the Cmd+F search bar over the active terminal tab. No-op
    /// when the active tab isn't a terminal.
    OpenTerminalSearch,
    /// Close the active terminal's search bar and drop the highlight.
    CloseTerminalSearch,
    /// User edited the search input for the active terminal.
    TerminalSearchQueryChanged(String),
    /// Jump to the next / previous match in the active terminal.
    TerminalSearchNext,
    TerminalSearchPrev,
}

impl BenchState {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active_tab: None,
            dropdown_open: false,
            terminal_search: HashMap::new(),
        }
    }

    /// Whether the active tab currently has the search overlay open.
    pub fn active_terminal_search(&self) -> Option<&TerminalSearchState> {
        self.active_tab.and_then(|id| self.terminal_search.get(&id))
    }

    /// Create a tab with an externally-assigned ID.
    pub fn create_tab(&mut self, id: u64, title: String, kind: TabKind) {
        self.tabs.push(Tab {
            id,
            title,
            kind,
            pinned: false,
        });
        self.active_tab = Some(id);
        self.dropdown_open = false;
    }

    pub fn close_tab(&mut self, id: u64) {
        if self.tabs.iter().any(|t| t.id == id && t.pinned) {
            return;
        }
        self.tabs.retain(|t| t.id != id);
        self.terminal_search.remove(&id);
        if self.active_tab == Some(id) {
            self.active_tab = self.tabs.last().map(|t| t.id);
        }
    }

    /// Returns true if the tab with the given id is pinned (cannot be closed).
    pub fn is_pinned(&self, id: u64) -> bool {
        self.tabs.iter().any(|t| t.id == id && t.pinned)
    }

    /// Render the tab bar row with liquid glass pill tabs.
    pub fn view_tab_bar(&self, pal: &Palette) -> Element<'_, Message> {
        let pal = *pal;
        let mut tab_row = row![].spacing(4).align_y(iced::Alignment::Center);

        for tab in &self.tabs {
            let is_active = self.active_tab == Some(tab.id);
            let text_color = if is_active {
                pal.text_primary
            } else {
                pal.text_secondary
            };

            let (kind_icon, tip_text) = match &tab.kind {
                TabKind::Terminal => ("\u{25B8}", "Terminal".to_string()),
                TabKind::CodingAgent(agent) => ("\u{2726}", agent.display_name.clone()),
                TabKind::FileViewer(path) => ("\u{25A2}", path.to_string_lossy().to_string()),
                TabKind::Home => ("\u{2302}", "Home".to_string()),
                TabKind::LogTail { log_path, .. } => {
                    ("\u{1F441}", format!("spy: {}", log_path.display()))
                }
            };

            let mut tab_content = row![
                text(kind_icon).size(FONT_ICON_SM).color(text_color),
                button(text(&tab.title).size(FONT_BODY).color(text_color))
                    .style(button::text)
                    .padding(0)
                    .on_press(Message::SelectTab(tab.id)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);

            if !tab.pinned {
                tab_content = tab_content.push(
                    button(text("\u{00D7}").size(FONT_ICON).color(pal.text_tertiary))
                        .style(button::text)
                        .padding(0)
                        .on_press(Message::CloseTab(tab.id)),
                );
            }

            let pill = container(tab_content)
                .padding([4, 10])
                .style(move |_theme: &Theme| style::tab_pill(&pal, is_active));

            tab_row = tab_row.push(
                tooltip(
                    pill,
                    text(tip_text).size(FONT_SMALL),
                    tooltip::Position::Bottom,
                )
                .gap(4)
                .style(move |_theme: &Theme| style::dropdown(&pal)),
            );
        }

        let new_btn = button(
            text("+  \u{25BE}")
                .size(FONT_ICON)
                .color(pal.text_secondary),
        )
        .style(button::text)
        .padding([4, 10])
        .on_press(Message::ToggleDropdown);

        // Wrap the tabs in a horizontal scrollable so long tab lists don't push
        // the "+" button offscreen. The scrollable fills the available width;
        // the "+" button stays pinned on the right, always reachable.
        let scrollable_tabs = scrollable(tab_row)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(4).scroller_width(4),
            ))
            .width(Length::Fill);

        row![scrollable_tabs, new_btn]
            .align_y(iced::Alignment::Center)
            .spacing(4)
            .padding([4, 8])
            .into()
    }

    /// Render the Cmd+F search overlay for the active terminal tab.
    /// Returns None when search is closed for the active tab. The
    /// overlay is meant to be stacked on top of the terminal view by
    /// the caller in `view_bench`.
    pub fn view_terminal_search<'a>(&'a self, pal: &Palette) -> Option<Element<'a, Message>> {
        let state = self.active_terminal_search()?;
        let pal = *pal;

        let input = text_input("Find in terminal", &state.query)
            .id(iced::widget::Id::new(TERMINAL_SEARCH_INPUT_ID))
            .size(13)
            .padding([4, 8])
            .on_input(Message::TerminalSearchQueryChanged)
            .on_submit(Message::TerminalSearchNext);

        let count_label: String = if state.query.is_empty() {
            String::new()
        } else if state.match_count == 0 {
            "no matches".to_string()
        } else {
            let cur = state.current_match.map(|i| i + 1).unwrap_or(0);
            format!("{} / {}", cur, state.match_count)
        };

        let prev_btn = button(text("\u{2191}").size(12))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::TerminalSearchPrev);

        let next_btn = button(text("\u{2193}").size(12))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::TerminalSearchNext);

        let close_btn = button(text("\u{2715}").size(12))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::CloseTerminalSearch);

        let bar = row![
            input,
            text(count_label).size(12).color(pal.text_secondary),
            prev_btn,
            next_btn,
            close_btn,
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);

        let card = container(bar)
            .padding([4, 8])
            .style(move |_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.55,
                })),
                border: iced::Border {
                    color: pal.border,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            });

        // Pin to the top-right of the terminal area, like browsers do.
        Some(
            row![Space::new().width(Length::Fill), card]
                .padding([6, 8])
                .width(Length::Fill)
                .into(),
        )
    }

    /// Render the dropdown menu (meant to be overlaid, not in flow).
    pub fn view_dropdown<'a>(
        &'a self,
        available_agents: &'a [CodingAgent],
        custom_agents: &'a [AgentDef],
        pal: &Palette,
    ) -> Option<Element<'a, Message>> {
        if !self.dropdown_open {
            return None;
        }

        let pal = *pal;
        let mut menu = column![].spacing(2).padding(6);

        // Home/overview is its own tab kind — single click opens or focuses
        // the existing instance. Always available; lives above the agent
        // entries because it's the first thing a user reaches for when they
        // want to see the big picture across many Hands.
        menu = menu.push(
            button(text("Open Home").size(FONT_BODY).color(pal.text_primary))
                .style(button::text)
                .width(Length::Fill)
                .on_press(Message::OpenHome),
        );

        // Spark ryve-acdb248a — Atlas is the **default entry point** for
        // top-level user requests. It is the first item in the dropdown
        // (immediately after Home) so the user's eye lands on it before
        // the bypass options. Atlas itself is a coding agent launched with
        // the Director system prompt; it delegates to Heads / Hands rather
        // than editing code, so the only thing the user has to choose at
        // this point is whether they want Atlas at all — agent selection
        // happens automatically (we pick the first compatible coding agent).
        // Users who want fine control bypass Atlas via New Hand / New Head /
        // New Terminal below.
        let any_agent_available = available_agents
            .iter()
            .any(|a| !a.compatibility.is_unsupported());

        let atlas_button = button(text("Ask Atlas...").size(FONT_BODY).color(
            if any_agent_available {
                pal.text_primary
            } else {
                pal.text_tertiary
            },
        ))
        .style(button::text)
        .width(Length::Fill);
        let atlas_button = if any_agent_available {
            atlas_button.on_press(Message::NewAtlas)
        } else {
            atlas_button
        };
        menu = menu.push(atlas_button);

        // ── Bypass paths (for advanced flows that skip Atlas) ──
        // Documented in docs/ATLAS.md.
        menu = menu.push(
            text("Bypass Atlas")
                .size(FONT_LABEL)
                .color(pal.text_tertiary),
        );

        let head_button = button(text("New Head...").size(FONT_BODY).color(
            if any_agent_available {
                pal.text_primary
            } else {
                pal.text_tertiary
            },
        ))
        .style(button::text)
        .width(Length::Fill);
        let head_button = if any_agent_available {
            head_button.on_press(Message::NewHead)
        } else {
            head_button
        };
        menu = menu.push(head_button);

        let hand_button = button(text("New Hand...").size(FONT_BODY).color(
            if any_agent_available {
                pal.text_primary
            } else {
                pal.text_tertiary
            },
        ))
        .style(button::text)
        .width(Length::Fill);
        let hand_button = if any_agent_available {
            hand_button.on_press(Message::NewHand)
        } else {
            hand_button
        };
        menu = menu.push(hand_button);

        menu = menu.push(
            button(
                text("New Terminal...")
                    .size(FONT_BODY)
                    .color(pal.text_primary),
            )
            .style(button::text)
            .width(Length::Fill)
            .on_press(Message::NewTerminal),
        );

        if !any_agent_available {
            menu = menu.push(
                text("(install claude/codex/aider/opencode to enable Head & Hand)")
                    .size(FONT_SMALL)
                    .color(pal.text_tertiary),
            );
        }

        // Custom agents still get their own quick-launch entries — they
        // bypass the picker because the user explicitly named the agent
        // when they registered it under .ryve/agents/.
        if !custom_agents.is_empty() {
            menu = menu.push(text("Custom").size(FONT_LABEL).color(pal.text_tertiary));
            for (i, def) in custom_agents.iter().enumerate() {
                let label = format!("New {}...", def.name);
                menu = menu.push(
                    button(text(label).size(FONT_BODY).color(pal.text_primary))
                        .style(button::text)
                        .width(Length::Fill)
                        .on_press(Message::NewCustomAgent(i)),
                );
            }
        }

        // Silence "unused parameter" warnings until the legacy direct-spawn
        // dropdown items are removed in a follow-up spark.
        let _ = available_agents;

        // Wrap the dropdown container in a mouse_area that swallows clicks
        // on the menu itself so the backdrop below doesn't immediately
        // close it. The backdrop (rendered in main.rs as a separate stack
        // layer) fires CloseDropdown for any click that misses the menu.
        let dropdown = mouse_area(
            container(menu)
                .style(move |_theme: &Theme| style::dropdown(&pal))
                .width(220),
        )
        .on_press(Message::NoOp);

        Some(
            row![Space::new().width(Length::Fill), dropdown]
                .width(Length::Fill)
                .into(),
        )
    }

    /// Render a full-size transparent backdrop that closes the dropdown
    /// when clicked. Returns `None` when the dropdown is closed so the
    /// backdrop doesn't swallow clicks the rest of the time.
    pub fn view_dropdown_backdrop<'a>(&self) -> Option<Element<'a, Message>> {
        if !self.dropdown_open {
            return None;
        }
        Some(
            mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
                .on_press(Message::CloseDropdown)
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Palette;

    #[test]
    fn tab_bar_handles_many_tabs_without_panic() {
        // Regression test for sp-ux0010: with many tabs the row used to push
        // the "+" button offscreen. The fix wraps the tabs in a horizontal
        // scrollable. This test ensures view_tab_bar still constructs cleanly
        // for a tab count well beyond the previous overflow threshold (~8).
        let mut bench = BenchState::new();
        for i in 0..50 {
            bench.create_tab(i, format!("tab-{i}"), TabKind::Terminal);
        }
        assert_eq!(bench.tabs.len(), 50);
        let pal = Palette::dark();
        let _element = bench.view_tab_bar(&pal);
    }

    #[test]
    fn tab_bar_renders_with_zero_tabs() {
        let bench = BenchState::new();
        let pal = Palette::dark();
        let _element = bench.view_tab_bar(&pal);
    }

    #[test]
    fn dropdown_backdrop_only_exists_when_open() {
        // sp-ux0022: the click-outside backdrop must only be rendered while
        // the dropdown is visible — otherwise it would swallow clicks to
        // the rest of the UI when no menu is up.
        let mut bench = BenchState::new();
        assert!(!bench.dropdown_open);
        assert!(bench.view_dropdown_backdrop().is_none());

        bench.dropdown_open = true;
        assert!(bench.view_dropdown_backdrop().is_some());

        bench.dropdown_open = false;
        assert!(bench.view_dropdown_backdrop().is_none());
    }

    #[test]
    fn close_tab_clears_terminal_search_entry() {
        // sp-ux0030: per-tab search state must follow tab lifetime —
        // closing the tab while search is open should drop the entry
        // so a future tab with the same id doesn't inherit it.
        let mut bench = BenchState::new();
        bench.create_tab(7, "term".into(), TabKind::Terminal);
        bench
            .terminal_search
            .insert(7, TerminalSearchState::default());
        assert!(bench.terminal_search.contains_key(&7));
        bench.close_tab(7);
        assert!(!bench.terminal_search.contains_key(&7));
    }

    /// Spark ryve-acdb248a — confirm `Message::NewAtlas` exists as a
    /// distinct routing variant. The default entry point for user-originated
    /// requests must produce this message so the app can dispatch it to the
    /// Atlas spawn handler instead of the bypass paths.
    #[test]
    fn new_atlas_message_variant_exists() {
        let m = Message::NewAtlas;
        assert!(matches!(m, Message::NewAtlas));
        // The bypass variants must remain available for advanced flows.
        assert!(matches!(Message::NewHead, Message::NewHead));
        assert!(matches!(Message::NewHand, Message::NewHand));
        assert!(matches!(Message::NewTerminal, Message::NewTerminal));
    }

    #[test]
    fn view_terminal_search_only_renders_when_active_tab_has_entry() {
        // sp-ux0030: the overlay is gated on the active tab having a
        // TerminalSearchState entry. No entry → no overlay.
        let mut bench = BenchState::new();
        bench.create_tab(1, "a".into(), TabKind::Terminal);
        bench.create_tab(2, "b".into(), TabKind::Terminal);
        let pal = Palette::dark();
        assert!(bench.view_terminal_search(&pal).is_none());
        // Open search on tab 1, but make tab 2 active — overlay must
        // stay hidden because the *active* tab has no entry.
        bench
            .terminal_search
            .insert(1, TerminalSearchState::default());
        bench.active_tab = Some(2);
        assert!(bench.view_terminal_search(&pal).is_none());
        // Switch to tab 1 — now the overlay appears.
        bench.active_tab = Some(1);
        assert!(bench.view_terminal_search(&pal).is_some());
    }

    /// Spark ryve-59983890 — pinned tabs cannot be closed.
    #[test]
    fn pinned_tab_is_not_closeable() {
        let mut bench = BenchState::new();
        bench.create_tab(1, "Atlas".into(), TabKind::Terminal);
        // Pin the tab
        bench.tabs[0].pinned = true;
        bench.create_tab(2, "other".into(), TabKind::Terminal);

        assert!(bench.is_pinned(1));
        assert!(!bench.is_pinned(2));

        // Attempting to close a pinned tab is a no-op
        bench.close_tab(1);
        assert_eq!(bench.tabs.len(), 2);
        assert!(bench.tabs.iter().any(|t| t.id == 1));

        // Non-pinned tab can still be closed
        bench.close_tab(2);
        assert_eq!(bench.tabs.len(), 1);
        assert!(!bench.tabs.iter().any(|t| t.id == 2));
    }
}
