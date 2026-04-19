// SPDX-License-Identifier: AGPL-3.0-or-later

//! IRC + GitHub detail / health view.
//!
//! Spark ryve-c3de335e: reachable via the status-bar gear icon (which
//! routes [`crate::screen::status_bar::Message::OpenIntegrations`]),
//! this screen surfaces the live state of both subsystems
//! (server/port/nick/connected for IRC; repo/mode/configured for GitHub)
//! plus a button to open the [`crate::screen::settings`] form. The
//! IRC/GitHub pills in the status bar are read-only indicators today
//! and do not themselves emit navigation messages (PR #49 Copilot c1).
//!
//! The view is rendered as a modal overlay. It reads only existing
//! [`data::ryve_dir::WorkshopConfig`] fields and a small connection
//! summary built by [`crate::workshop::Workshop`] from the live
//! `IrcRuntime`. When IRC or GitHub are disabled / unconfigured the
//! view degrades gracefully — showing a "Disabled" or "Unconfigured"
//! row with a link to the settings form rather than crashing or
//! hiding the section entirely.

use data::ryve_dir::{GitHubConfig, WorkshopConfig};
use iced::widget::{Space, button, column, container, row, rule, text};
use iced::{Element, Length, Theme};

use crate::style::{self, FONT_BODY, FONT_HEADER, FONT_LABEL, FONT_SMALL, Palette};

// ── Messages ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    /// Close the overlay.
    Close,
    /// Open the settings form so the user can edit credentials/server.
    OpenSettings,
}

// ── Health snapshots ──────────────────────────────────

/// Live IRC runtime status sampled by the workshop. Keeps the screen
/// view function pure — the real `IrcRuntime` lives behind a
/// `std::sync::Mutex` inside the workshop, and we don't want the
/// renderer to acquire it. (PR #49 Copilot c2: was previously
/// described as a tokio `Mutex` which implied async-aware locking.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrcStatus {
    /// `irc_server` is unset — subsystem dormant.
    Disabled,
    /// `irc_server` is set but `IrcRuntime::start` hasn't completed.
    /// Either the boot task is still running or the last connect attempt
    /// failed. The detail view shows the configured server so the user
    /// can verify it.
    Configured,
    /// `IrcRuntime` is running. `known_channels` is the count of epic
    /// channels we've ensured since boot.
    Connected { known_channels: usize },
}

/// Snapshot fed to [`view`] for the IRC section.
#[derive(Debug, Clone)]
pub struct IrcHealth {
    pub status: IrcStatus,
    pub server: Option<String>,
    pub port: u16,
    pub tls: bool,
    pub nick: String,
}

/// Snapshot fed to [`view`] for the GitHub section.
#[derive(Debug, Clone)]
pub struct GitHubHealth {
    pub repo: Option<String>,
    pub mode: GitHubMode,
    pub auto_sync: bool,
    pub configured: bool,
}

/// Which ingestion path the artifact mirror will take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHubMode {
    /// `webhook_secret` is set — the poller stays disabled.
    Webhook,
    /// No webhook configured but a poll/legacy token is present.
    Polling,
    /// No credentials at all.
    Unconfigured,
}

impl GitHubMode {
    /// Pick the mode for the given GitHub config. Webhook wins when
    /// both are configured, mirroring [`data::github::poller::PollerConfig`]
    /// — when a webhook is wired up the poller must not double-ingest.
    pub fn from_config(github: &GitHubConfig) -> Self {
        if github.webhook_configured() {
            Self::Webhook
        } else if github.poll_token_configured() {
            Self::Polling
        } else {
            Self::Unconfigured
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Webhook => "Webhook ingestion",
            Self::Polling => "REST polling",
            Self::Unconfigured => "Not configured",
        }
    }
}

/// Build [`IrcHealth`] from the workshop config plus a runtime-known
/// "is there a live IrcRuntime?" flag and the channel count cached on
/// the workshop. Pure function so it can be unit-tested.
pub fn irc_health_from(
    config: &WorkshopConfig,
    runtime_active: bool,
    known_channels: usize,
) -> IrcHealth {
    let status = if !config.irc_enabled() {
        IrcStatus::Disabled
    } else if runtime_active {
        IrcStatus::Connected { known_channels }
    } else {
        IrcStatus::Configured
    };
    IrcHealth {
        status,
        server: config.irc_server.clone(),
        port: config.effective_irc_port(),
        tls: config.irc_tls.unwrap_or(false),
        nick: config.effective_irc_nick(),
    }
}

/// Build [`GitHubHealth`] from the workshop config. Pure function for
/// testability.
pub fn github_health_from(config: &WorkshopConfig) -> GitHubHealth {
    GitHubHealth {
        repo: config.github.repo.clone(),
        mode: GitHubMode::from_config(&config.github),
        auto_sync: config.github.auto_sync,
        configured: config.github.is_configured(),
    }
}

// ── View ──────────────────────────────────────────────

pub fn view(irc: &IrcHealth, github: &GitHubHealth, pal: &Palette) -> Element<'static, Message> {
    let pal = *pal;

    let title = text("Integrations")
        .size(FONT_HEADER)
        .color(pal.text_primary);
    let subtitle = text("Live state of the workshop's coordination back-ends.")
        .size(FONT_SMALL)
        .color(pal.text_tertiary);
    let close_btn = button(text("\u{00D7}").size(FONT_HEADER).color(pal.text_secondary))
        .style(button::text)
        .padding([2, 8])
        .on_press(Message::Close);

    let header = row![
        column![title, subtitle].spacing(2),
        Space::new().width(Length::Fill),
        close_btn,
    ]
    .align_y(iced::Alignment::Center);

    // ── IRC section ───────────────────────────────────
    let irc_status_label = irc_status_label(&irc.status);
    let irc_status_color = match irc.status {
        IrcStatus::Connected { .. } => pal.success,
        IrcStatus::Configured => pal.text_secondary,
        IrcStatus::Disabled => pal.text_tertiary,
    };
    let irc_heading = row![
        text("IRC").size(FONT_BODY).color(pal.text_primary),
        Space::new().width(Length::Fill),
        text(irc_status_label)
            .size(FONT_LABEL)
            .color(irc_status_color),
    ]
    .align_y(iced::Alignment::Center);

    let mut irc_body = column![irc_heading].spacing(6);
    match &irc.status {
        IrcStatus::Disabled => {
            irc_body = irc_body.push(
                text("No server address is configured. Open settings to enable IRC.")
                    .size(FONT_SMALL)
                    .color(pal.text_secondary),
            );
        }
        IrcStatus::Configured | IrcStatus::Connected { .. } => {
            let server = irc
                .server
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("(unset)");
            irc_body = irc_body.push(detail_row(
                "Server",
                &format!(
                    "{}:{}{}",
                    server,
                    irc.port,
                    if irc.tls { "  (TLS)" } else { "" }
                ),
                &pal,
            ));
            irc_body = irc_body.push(detail_row("Nick", &irc.nick, &pal));
            if let IrcStatus::Connected { known_channels } = irc.status {
                irc_body = irc_body.push(detail_row(
                    "Joined channels",
                    &format!(
                        "{} epic channel{}",
                        known_channels,
                        if known_channels == 1 { "" } else { "s" }
                    ),
                    &pal,
                ));
            } else {
                irc_body = irc_body.push(
                    text("Connecting (or last attempt failed; check the toast log).")
                        .size(FONT_SMALL)
                        .color(pal.text_tertiary),
                );
            }
        }
    }

    // ── GitHub section ────────────────────────────────
    let gh_status_color = match github.mode {
        GitHubMode::Webhook => pal.success,
        GitHubMode::Polling => pal.accent,
        GitHubMode::Unconfigured => pal.text_tertiary,
    };
    let gh_heading = row![
        text("GitHub").size(FONT_BODY).color(pal.text_primary),
        Space::new().width(Length::Fill),
        text(github.mode.label())
            .size(FONT_LABEL)
            .color(gh_status_color),
    ]
    .align_y(iced::Alignment::Center);

    let mut gh_body = column![gh_heading].spacing(6);
    let repo = github
        .repo
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("(unset)");
    gh_body = gh_body.push(detail_row("Repository", repo, &pal));
    gh_body = gh_body.push(detail_row(
        "Mode",
        match github.mode {
            GitHubMode::Webhook => "Webhook (poller disabled)",
            GitHubMode::Polling => "REST polling fallback",
            GitHubMode::Unconfigured => "No credentials wired up",
        },
        &pal,
    ));
    gh_body = gh_body.push(detail_row(
        "Auto-sync",
        if github.auto_sync {
            "Enabled"
        } else {
            "Disabled"
        },
        &pal,
    ));
    if !github.configured {
        gh_body = gh_body.push(
            text("Open settings to add a webhook secret or poll token.")
                .size(FONT_SMALL)
                .color(pal.text_secondary),
        );
    }

    // ── Footer ────────────────────────────────────────
    let edit_btn = button(
        text("Edit settings…")
            .size(FONT_LABEL)
            .color(pal.text_primary),
    )
    .style(button::secondary)
    .padding([6, 14])
    .on_press(Message::OpenSettings);
    let footer = row![Space::new().width(Length::Fill), edit_btn].align_y(iced::Alignment::Center);

    let body = column![
        header,
        rule::horizontal(1),
        irc_body,
        rule::horizontal(1),
        gh_body,
        rule::horizontal(1),
        footer,
    ]
    .spacing(12)
    .padding(20)
    .width(Length::Fixed(520.0));

    let inner = container(body).style(move |_t: &Theme| style::modal(&pal));

    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(move |_t: &Theme| style::modal_backdrop(&pal))
        .into()
}

fn detail_row<'a>(label: &str, value: &str, pal: &Palette) -> Element<'a, Message> {
    row![
        container(
            text(label.to_string())
                .size(FONT_SMALL)
                .color(pal.text_secondary)
        )
        .width(Length::Fixed(140.0)),
        text(value.to_string())
            .size(FONT_LABEL)
            .color(pal.text_primary),
    ]
    .align_y(iced::Alignment::Center)
    .into()
}

/// User-facing label for an [`IrcStatus`]. Pulled out for tests.
pub fn irc_status_label(status: &IrcStatus) -> &'static str {
    match status {
        IrcStatus::Disabled => "Disabled",
        IrcStatus::Configured => "Configured",
        IrcStatus::Connected { .. } => "Connected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_mode_unconfigured_when_nothing_set() {
        assert_eq!(
            GitHubMode::from_config(&GitHubConfig::default()),
            GitHubMode::Unconfigured
        );
    }

    #[test]
    fn github_mode_polling_when_only_token_set() {
        let cfg = GitHubConfig {
            poll_token: Some("ghp_xxx".into()),
            ..Default::default()
        };
        assert_eq!(GitHubMode::from_config(&cfg), GitHubMode::Polling);
    }

    #[test]
    fn github_mode_polling_falls_back_to_legacy_token() {
        let cfg = GitHubConfig {
            token: Some("ghp_legacy".into()),
            ..Default::default()
        };
        assert_eq!(GitHubMode::from_config(&cfg), GitHubMode::Polling);
    }

    #[test]
    fn github_mode_webhook_wins_when_both_configured() {
        let cfg = GitHubConfig {
            webhook_secret: Some("shh".into()),
            poll_token: Some("ghp_xxx".into()),
            ..Default::default()
        };
        assert_eq!(GitHubMode::from_config(&cfg), GitHubMode::Webhook);
    }

    #[test]
    fn irc_health_disabled_when_no_server() {
        let cfg = WorkshopConfig::default();
        let h = irc_health_from(&cfg, false, 0);
        assert_eq!(h.status, IrcStatus::Disabled);
    }

    #[test]
    fn irc_health_configured_when_server_set_but_runtime_inactive() {
        let cfg = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            ..Default::default()
        };
        let h = irc_health_from(&cfg, false, 0);
        assert_eq!(h.status, IrcStatus::Configured);
        assert_eq!(h.server.as_deref(), Some("irc.example.com"));
        assert_eq!(h.port, 6667);
        assert!(!h.tls);
    }

    #[test]
    fn irc_health_connected_includes_channel_count() {
        let cfg = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            irc_tls: Some(true),
            ..Default::default()
        };
        let h = irc_health_from(&cfg, true, 4);
        assert_eq!(h.status, IrcStatus::Connected { known_channels: 4 });
        assert_eq!(h.port, 6697);
        assert!(h.tls);
    }

    #[test]
    fn irc_health_disabled_overrides_runtime_active() {
        // Defensive: if irc_server is unset we should never claim to
        // be connected, even if a stale IrcRuntime was somehow held.
        let cfg = WorkshopConfig::default();
        let h = irc_health_from(&cfg, true, 7);
        assert_eq!(h.status, IrcStatus::Disabled);
    }

    #[test]
    fn github_health_reports_configured_state() {
        let cfg = WorkshopConfig {
            github: GitHubConfig {
                repo: Some("octo/cat".into()),
                webhook_secret: Some("shh".into()),
                auto_sync: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let h = github_health_from(&cfg);
        assert_eq!(h.repo.as_deref(), Some("octo/cat"));
        assert_eq!(h.mode, GitHubMode::Webhook);
        assert!(h.auto_sync);
        assert!(h.configured);
    }

    #[test]
    fn github_health_unconfigured_default() {
        let h = github_health_from(&WorkshopConfig::default());
        assert_eq!(h.mode, GitHubMode::Unconfigured);
        assert!(!h.configured);
        assert!(!h.auto_sync);
    }

    #[test]
    fn irc_status_labels_are_distinct() {
        assert_eq!(irc_status_label(&IrcStatus::Disabled), "Disabled");
        assert_eq!(irc_status_label(&IrcStatus::Configured), "Configured");
        assert_eq!(
            irc_status_label(&IrcStatus::Connected { known_channels: 0 }),
            "Connected"
        );
    }
}
