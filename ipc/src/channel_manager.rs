// SPDX-License-Identifier: AGPL-3.0-or-later

//! IRC channel manager for Ryve epics.
//!
//! Epics map 1:1 onto IRC channels. This module owns the canonical
//! channel-name derivation plus the two lifecycle operations the relay
//! needs: making sure the channel exists and its topic is current
//! ([`ensure_channel`]), and joining an actor to the channel
//! ([`register_actor`]). Every caller that derives a channel name from
//! an epic must route through [`channel_name`] so the name is identical
//! in every subsystem (relay, UI, tests).
//!
//! The channel-name format is `#epic-<id>-<slug>` where `slug` is the
//! epic name lowercased and slugified, then the whole string is
//! truncated to the IRC 50-octet channel-name limit (RFC 2812 §1.3).
//!
//! Idempotency: both [`ensure_channel`] and [`register_actor`] are safe
//! to call repeatedly. Joining a channel already joined is a local
//! no-op in [`crate::irc_client::IrcClient`] (the command is still
//! sent, but the server-side effect of re-joining an already-joined
//! channel is harmless). Setting the topic to the same value is a
//! no-op at the semantic level.

use crate::irc_client::{IrcClient, IrcError};

/// IRC channel names are capped at 50 octets including the `#` prefix
/// (RFC 2812 §1.3). Longer names are silently truncated by some servers
/// and outright rejected by others, so [`channel_name`] clamps itself.
pub const IRC_MAX_CHANNEL_LEN: usize = 50;

/// Identifying fields of an epic used for channel naming. Kept minimal
/// so call sites that have only an id + name (the renderer, the UI)
/// don't need to supply status-like fields they don't know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpicRef {
    pub id: String,
    pub name: String,
}

/// Full epic view used by [`ensure_channel`] to set the channel topic.
/// The `status` string is rendered verbatim — the caller chooses how
/// epic status is spelled (`open`, `in_progress`, `closed: completed`,
/// etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Epic {
    pub id: String,
    pub name: String,
    pub status: String,
}

impl Epic {
    pub fn as_ref(&self) -> EpicRef {
        EpicRef {
            id: self.id.clone(),
            name: self.name.clone(),
        }
    }
}

/// Actor joining an epic channel. Today the struct is just an id —
/// the nick is taken from the [`IrcClient`]'s own config when the
/// server echoes the JOIN — but it's a type so future fields (display
/// name, role) can land without changing `register_actor`'s signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub id: String,
}

impl Actor {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

/// Derive the canonical IRC channel name for an epic.
///
/// Shape: `#epic-<id>-<slug>`. When the epic name slugifies to an
/// empty string (all symbols / whitespace), the trailing dash + slug
/// are dropped and the channel is simply `#epic-<id>`. The final
/// string is truncated to [`IRC_MAX_CHANNEL_LEN`] octets with any
/// trailing dashes left over by truncation stripped.
pub fn channel_name(epic: &EpicRef) -> String {
    let slug = slugify(&epic.name);
    let raw = if slug.is_empty() {
        format!("#epic-{}", epic.id)
    } else {
        format!("#epic-{}-{}", epic.id, slug)
    };
    truncate_channel(&raw)
}

fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = true; // suppress leading dashes
    for ch in name.chars() {
        let normalised = ch.to_ascii_lowercase();
        if normalised.is_ascii_alphanumeric() {
            out.push(normalised);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

fn truncate_channel(channel: &str) -> String {
    if channel.len() <= IRC_MAX_CHANNEL_LEN {
        return channel.to_string();
    }
    // slugify produces ASCII so byte-truncation is safe here, but guard
    // the id half against rare non-ASCII ids by advancing on char
    // boundaries only.
    let mut end = IRC_MAX_CHANNEL_LEN;
    while !channel.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = channel[..end].to_string();
    while truncated.ends_with('-') {
        truncated.pop();
    }
    truncated
}

/// Make sure the epic's channel exists on the server and its topic
/// reflects the current epic status.
///
/// Effect:
/// 1. JOIN the channel (idempotent — already-joined is a no-op).
/// 2. Set the TOPIC to `"<name> — <status>"`.
///
/// A fresh call after a reconnect is safe: `IrcClient` auto-rejoins
/// channels it has been asked to join, and setting the topic to the
/// current value is a semantic no-op.
pub async fn ensure_channel(client: &IrcClient, epic: &Epic) -> Result<(), IrcError> {
    let channel = channel_name(&epic.as_ref());
    client.join(&channel).await?;
    let topic = format!("{} \u{2014} {}", epic.name, epic.status);
    client.set_topic(&channel, &topic).await?;
    Ok(())
}

/// Join `actor`'s IRC client to the epic's channel. The `actor`
/// argument is carried through for future auditing / logging; the
/// join itself is keyed on the epic.
pub async fn register_actor(
    client: &IrcClient,
    _actor: &Actor,
    epic: &EpicRef,
) -> Result<(), IrcError> {
    let channel = channel_name(epic);
    client.join(&channel).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epic(id: &str, name: &str) -> EpicRef {
        EpicRef {
            id: id.into(),
            name: name.into(),
        }
    }

    #[test]
    fn channel_name_uses_id_and_lowercase_slug() {
        assert_eq!(
            channel_name(&epic("42", "Checkout Refactor")),
            "#epic-42-checkout-refactor"
        );
    }

    #[test]
    fn channel_name_collapses_runs_of_non_alphanumeric() {
        assert_eq!(
            channel_name(&epic("9", "  Cart // Bug --- Fix!! ")),
            "#epic-9-cart-bug-fix"
        );
    }

    #[test]
    fn channel_name_strips_special_chars() {
        assert_eq!(
            channel_name(&epic("1", "hello@world!#$%^&*()")),
            "#epic-1-hello-world"
        );
    }

    #[test]
    fn channel_name_handles_unicode_by_dashing() {
        assert_eq!(channel_name(&epic("7", "résumé café")), "#epic-7-r-sum-caf");
    }

    #[test]
    fn channel_name_with_empty_slug_drops_trailing_dash() {
        assert_eq!(channel_name(&epic("13", "!!!")), "#epic-13");
        assert_eq!(channel_name(&epic("14", "   ")), "#epic-14");
        assert_eq!(channel_name(&epic("15", "")), "#epic-15");
    }

    #[test]
    fn channel_name_truncates_long_names_to_irc_limit() {
        let long_name = "a".repeat(200);
        let ch = channel_name(&epic("42", &long_name));
        assert!(
            ch.len() <= IRC_MAX_CHANNEL_LEN,
            "channel {ch:?} exceeds IRC limit"
        );
        assert!(ch.starts_with("#epic-42-"));
        // All remaining slug chars must be 'a'.
        assert!(ch.trim_start_matches("#epic-42-").chars().all(|c| c == 'a'));
    }

    #[test]
    fn channel_name_truncation_strips_trailing_dash() {
        // Craft a name whose slug has a dash exactly at the truncation
        // boundary; after cutting, we should not see a trailing '-'.
        //
        // prefix "#epic-42-" is 9 bytes; IRC_MAX_CHANNEL_LEN - 9 = 41.
        // Make the 41st slug char a dash by emitting 40 'a's, then a
        // non-alphanumeric, then more 'a's.
        let mut name = String::new();
        name.push_str(&"a".repeat(40));
        name.push('!');
        name.push_str(&"a".repeat(50));
        let ch = channel_name(&epic("42", &name));
        assert!(!ch.ends_with('-'), "channel {ch:?} ends with dash");
        assert!(ch.len() <= IRC_MAX_CHANNEL_LEN);
    }

    #[test]
    fn channel_name_truncation_preserves_char_boundaries() {
        // A multi-byte id half with a slug that crosses the boundary
        // must still be a valid UTF-8 string, not a panic.
        let long_name = "é".repeat(100); // é is 2 bytes
        let ch = channel_name(&epic("é", &long_name));
        assert!(ch.is_char_boundary(ch.len()));
        assert!(ch.len() <= IRC_MAX_CHANNEL_LEN);
    }

    #[test]
    fn channel_name_is_deterministic() {
        let e = epic("42", "Checkout Refactor");
        let a = channel_name(&e);
        let b = channel_name(&e);
        assert_eq!(a, b);
    }

    #[test]
    fn channel_name_is_case_insensitive_on_slug() {
        assert_eq!(
            channel_name(&epic("42", "CHECKOUT REFACTOR")),
            channel_name(&epic("42", "checkout refactor")),
        );
    }

    #[test]
    fn actor_constructor_stores_id() {
        let a = Actor::new("agent_claude_01");
        assert_eq!(a.id, "agent_claude_01");
    }

    #[test]
    fn epic_as_ref_round_trip() {
        let e = Epic {
            id: "7".into(),
            name: "N".into(),
            status: "open".into(),
        };
        let r = e.as_ref();
        assert_eq!(r.id, "7");
        assert_eq!(r.name, "N");
    }
}
