// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workshop integrations settings form.
//!
//! Spark ryve-c3de335e: lets the user set `irc_server` (serialised as
//! the `irc_server` key in `.ryve/config.toml`) and the GitHub
//! credentials (`webhook_secret`, `poll_token`) without hand-editing
//! the TOML. Persistence is dispatched by the parent app
//! (`Message::Settings(_)` → `WorkshopConfig` write) once the user
//! commits a field. (PR #49 Copilot c3: earlier revisions called the
//! field `irc.server_address` which does not exist on
//! [`WorkshopConfig`].)
//!
//! The screen is rendered as a modal overlay. Navigation comes via
//! [`crate::workshop::Workshop`] for workshop/integrations flows,
//! including the cross-link from the Integrations detail screen
//! (PR #49 Copilot c11: the status-bar gear icon itself opens the
//! Integrations health overlay first, not this screen directly).

use data::ryve_dir::{GitHubConfig, WorkshopConfig};
use iced::widget::{Space, button, column, container, row, rule, text, text_input};
use iced::{Element, Length, Theme};

use crate::style::{self, FONT_BODY, FONT_HEADER, FONT_LABEL, FONT_SMALL, Palette};

// ── Messages ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    /// Close the settings overlay without further action.
    Close,
    /// IRC server text input changed (live, not yet persisted).
    IrcServerChanged(String),
    /// IRC server input committed (Enter / blur). The app handler
    /// trims and writes the value, clearing it back to `None` if blank.
    IrcServerSubmitted,
    /// GitHub webhook secret input changed.
    WebhookSecretChanged(String),
    /// GitHub webhook secret committed.
    WebhookSecretSubmitted,
    /// GitHub poll token input changed.
    PollTokenChanged(String),
    /// GitHub poll token committed.
    PollTokenSubmitted,
    /// Open the Integrations detail screen.
    ShowIntegrations,
}

// ── State ─────────────────────────────────────────────

/// Form drafts for the settings screen. Lives on
/// [`crate::workshop::Workshop`] alongside the other panel state so it
/// survives view re-renders without being rebuilt every frame.
#[derive(Debug, Default, Clone)]
pub struct SettingsFormState {
    pub irc_server_draft: String,
    pub webhook_secret_draft: String,
    pub poll_token_draft: String,
}

impl SettingsFormState {
    /// Hydrate the drafts from the workshop config. Called when the
    /// settings overlay is opened so the form starts from the latest
    /// persisted values.
    pub fn seed_from(&mut self, config: &WorkshopConfig) {
        self.irc_server_draft = config.irc_server.clone().unwrap_or_default();
        self.webhook_secret_draft = config.github.webhook_secret.clone().unwrap_or_default();
        self.poll_token_draft = config.github.poll_token.clone().unwrap_or_default();
    }

    /// Apply only the IRC server draft; used when the user commits one
    /// field at a time so we don't write unrelated drafts to disk.
    pub fn apply_irc_server(&self, config: &mut WorkshopConfig) -> bool {
        let new_irc = trim_to_optional(&self.irc_server_draft);
        if new_irc == config.irc_server {
            return false;
        }
        config.irc_server = new_irc;
        true
    }

    /// Apply only the GitHub webhook secret draft.
    pub fn apply_webhook_secret(&self, config: &mut WorkshopConfig) -> bool {
        let new_webhook = trim_to_optional(&self.webhook_secret_draft);
        if new_webhook == config.github.webhook_secret {
            return false;
        }
        config.github.webhook_secret = new_webhook;
        true
    }

    /// Apply only the GitHub poll token draft.
    pub fn apply_poll_token(&self, config: &mut WorkshopConfig) -> bool {
        let new_poll = trim_to_optional(&self.poll_token_draft);
        if new_poll == config.github.poll_token {
            return false;
        }
        config.github.poll_token = new_poll;
        true
    }
}

fn trim_to_optional(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// ── View ──────────────────────────────────────────────

/// Render the settings form as a modal overlay. `state` holds the
/// editable draft values shown in each input, `github` provides the
/// persisted GitHub settings used for the "currently configured" hint
/// alongside each field, and `pal` is the active palette.
/// (PR #49 Copilot c6: earlier revisions mentioned a `_config`
/// parameter which was never part of the signature.)
pub fn view<'a>(
    state: &'a SettingsFormState,
    github: &'a GitHubConfig,
    pal: &Palette,
) -> Element<'a, Message> {
    let pal = *pal;

    // ── Header ────────────────────────────────────────
    let title = text("Workshop Integrations")
        .size(FONT_HEADER)
        .color(pal.text_primary);
    let subtitle = text("Edit the values stored in .ryve/config.toml.")
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
    let irc_heading = text("IRC server").size(FONT_BODY).color(pal.text_primary);
    // PR #50 Copilot c5: with bundled IRC, a blank field no longer
    // means "disabled" — it falls back to the workshop-local
    // daemon at 127.0.0.1:<bundled_port> that `ryve init` provisions.
    // Users who really want to disable IRC flip `irc_enabled = false`
    // in `.ryve/config.toml` (see house rules / integrations docs).
    let irc_hint = text(
        "Override for the coordination server (e.g. irc.libera.chat:6697). \
         Leave blank to use the bundled workshop-local daemon. To disable \
         IRC entirely, set irc_enabled = false in .ryve/config.toml.",
    )
    .size(FONT_SMALL)
    .color(pal.text_secondary);
    let irc_input = text_input("irc.example.com", &state.irc_server_draft)
        .on_input(Message::IrcServerChanged)
        .on_submit(Message::IrcServerSubmitted)
        .size(FONT_BODY)
        .padding(8);
    let irc_save = button(text("Save").size(FONT_LABEL).color(pal.text_primary))
        .style(button::secondary)
        .padding([4, 12])
        .on_press(Message::IrcServerSubmitted);
    let irc_row = row![irc_input, irc_save]
        .spacing(8)
        .align_y(iced::Alignment::Center);

    // ── GitHub section ────────────────────────────────
    let gh_heading = text("GitHub credentials")
        .size(FONT_BODY)
        .color(pal.text_primary);
    let gh_hint = text(format!(
        "Repository: {}",
        github
            .repo
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("(not set)"),
    ))
    .size(FONT_SMALL)
    .color(pal.text_secondary);

    let webhook_label = text("Webhook secret")
        .size(FONT_LABEL)
        .color(pal.text_secondary);
    let webhook_input = text_input(
        "shared secret used to verify webhook payloads",
        &state.webhook_secret_draft,
    )
    .on_input(Message::WebhookSecretChanged)
    .on_submit(Message::WebhookSecretSubmitted)
    .secure(true)
    .size(FONT_BODY)
    .padding(8);
    let webhook_save = button(text("Save").size(FONT_LABEL).color(pal.text_primary))
        .style(button::secondary)
        .padding([4, 12])
        .on_press(Message::WebhookSecretSubmitted);
    let webhook_row = row![webhook_input, webhook_save]
        .spacing(8)
        .align_y(iced::Alignment::Center);

    let poll_label = text("Poll token")
        .size(FONT_LABEL)
        .color(pal.text_secondary);
    let poll_input = text_input(
        "PAT used by the REST polling fallback",
        &state.poll_token_draft,
    )
    .on_input(Message::PollTokenChanged)
    .on_submit(Message::PollTokenSubmitted)
    .secure(true)
    .size(FONT_BODY)
    .padding(8);
    let poll_save = button(text("Save").size(FONT_LABEL).color(pal.text_primary))
        .style(button::secondary)
        .padding([4, 12])
        .on_press(Message::PollTokenSubmitted);
    let poll_row = row![poll_input, poll_save]
        .spacing(8)
        .align_y(iced::Alignment::Center);

    // ── Footer ────────────────────────────────────────
    let see_health = button(
        text("See integration health \u{2192}")
            .size(FONT_LABEL)
            .color(pal.accent),
    )
    .style(button::text)
    .padding([4, 0])
    .on_press(Message::ShowIntegrations);

    let footer =
        row![Space::new().width(Length::Fill), see_health].align_y(iced::Alignment::Center);

    let body = column![
        header,
        rule::horizontal(1),
        irc_heading,
        irc_hint,
        irc_row,
        rule::horizontal(1),
        gh_heading,
        gh_hint,
        webhook_label,
        webhook_row,
        poll_label,
        poll_row,
        rule::horizontal(1),
        footer,
    ]
    .spacing(10)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_pulls_values_from_config() {
        let mut state = SettingsFormState::default();
        let cfg = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            github: GitHubConfig {
                webhook_secret: Some("shh".into()),
                poll_token: Some("ghp_xxx".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        state.seed_from(&cfg);
        assert_eq!(state.irc_server_draft, "irc.example.com");
        assert_eq!(state.webhook_secret_draft, "shh");
        assert_eq!(state.poll_token_draft, "ghp_xxx");
    }

    #[test]
    fn seed_handles_empty_config() {
        let mut state = SettingsFormState::default();
        let cfg = WorkshopConfig::default();
        state.seed_from(&cfg);
        assert!(state.irc_server_draft.is_empty());
        assert!(state.webhook_secret_draft.is_empty());
        assert!(state.poll_token_draft.is_empty());
    }

    #[test]
    fn apply_irc_server_clears_field_when_draft_blank() {
        let mut cfg = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            ..Default::default()
        };
        let state = SettingsFormState {
            irc_server_draft: "   ".into(),
            ..Default::default()
        };
        assert!(state.apply_irc_server(&mut cfg));
        assert!(cfg.irc_server.is_none());
    }

    #[test]
    fn per_field_apply_only_touches_that_field() {
        let mut cfg = WorkshopConfig::default();
        let state = SettingsFormState {
            irc_server_draft: "irc.example.com".into(),
            webhook_secret_draft: "shh".into(),
            poll_token_draft: "ghp".into(),
        };

        assert!(state.apply_irc_server(&mut cfg));
        assert_eq!(cfg.irc_server.as_deref(), Some("irc.example.com"));
        assert!(cfg.github.webhook_secret.is_none());
        assert!(cfg.github.poll_token.is_none());

        assert!(state.apply_webhook_secret(&mut cfg));
        assert_eq!(cfg.github.webhook_secret.as_deref(), Some("shh"));
        assert!(cfg.github.poll_token.is_none());

        assert!(state.apply_poll_token(&mut cfg));
        assert_eq!(cfg.github.poll_token.as_deref(), Some("ghp"));

        // Re-applying the same drafts is a no-op.
        assert!(!state.apply_irc_server(&mut cfg));
        assert!(!state.apply_webhook_secret(&mut cfg));
        assert!(!state.apply_poll_token(&mut cfg));
    }

    #[test]
    fn trim_to_optional_blank_becomes_none() {
        assert_eq!(trim_to_optional(""), None);
        assert_eq!(trim_to_optional("   "), None);
        assert_eq!(trim_to_optional(" abc "), Some("abc".to_string()));
    }
}
