// SPDX-License-Identifier: AGPL-3.0-or-later

//! `chat.post` and `chat.tail` MCP tool wrappers.
//!
//! Both tools are thin passthroughs to [`ipc::chat_of_record`] — the
//! same primitives `ryve post` and `ryve channel tail` call. The MCP
//! layer owns only JSON-in / JSON-out marshalling and schema metadata;
//! channel→epic resolution, limit bounds, and the DB-write-is-contract
//! semantics all live in the wrapped module.
//!
//! ## Output shape parity
//!
//! `chat.tail`'s output is a JSON array of `irc_messages` rows — the
//! same payload the CLI emits under `ryve channel tail --json`. Keeping
//! the two shapes aligned means an MCP-driven agent and a shell-driven
//! operator see identical data without a separate deserialiser per
//! transport.

use data::sparks::types::IrcMessage;
use ipc::chat_of_record::{self, NewPost, TAIL_DEFAULT_LIMIT, TailFilter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::{McpToolError, ToolDescriptor};

/// Dotted tool name exposed over MCP. Kept as a constant so the
/// dispatcher and descriptor stay in sync without string literals
/// scattered across the module.
pub const POST_NAME: &str = "chat.post";
pub const TAIL_NAME: &str = "chat.tail";

/// Typed input for [`post`]. The `#[serde(deny_unknown_fields)]`
/// attribute gives us schema-strict validation for free — unknown
/// keys become [`McpToolError::InvalidInput`] rather than silently
/// ignored typos.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatPostInput {
    pub channel: String,
    pub body: String,
    #[serde(default)]
    pub author_session_id: Option<String>,
    #[serde(default)]
    pub epic_id: Option<String>,
}

/// Typed output for [`post`]. Mirrors the CLI's `--json` shape for
/// `ryve post`: `{ "id": <row id>, "channel": <name> }`.
#[derive(Debug, Serialize)]
pub struct ChatPostOutput {
    pub id: i64,
    pub channel: String,
}

/// Typed input for [`tail`]. `limit` is optional so clients can rely
/// on [`TAIL_DEFAULT_LIMIT`] without having to hard-code it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatTailInput {
    pub channel: String,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub author_session_id: Option<String>,
}

/// Descriptor for the `chat.post` tool.
pub fn post_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: POST_NAME,
        description: "Persist a chat-of-record post to an epic channel. Writes to \
                      irc_messages (durable); IRC wire delivery is best-effort \
                      and not awaited here.",
        input_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Canonical #epic-<id>-<slug> channel, with leading '#'."
                },
                "body": {
                    "type": "string",
                    "description": "Free-form post body; stored as-is."
                },
                "author_session_id": {
                    "type": ["string", "null"],
                    "description": "agent_sessions.id of the author; null for unattributed posts."
                },
                "epic_id": {
                    "type": ["string", "null"],
                    "description": "Optional override. When null the epic id is resolved from the channel."
                }
            },
            "required": ["channel", "body"],
            "additionalProperties": false
        }),
        output_schema: json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer", "description": "Primary key of the inserted irc_messages row." },
                "channel": { "type": "string" }
            },
            "required": ["id", "channel"]
        }),
    }
}

/// Descriptor for the `chat.tail` tool.
pub fn tail_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TAIL_NAME,
        description: "Read a bounded window of chat-of-record posts from a channel, \
                      optionally filtered by `since` and `author_session_id`. \
                      Returns rows in the same JSON shape as `ryve channel tail --json`.",
        input_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Canonical #epic-<id>-<slug> channel, with leading '#'."
                },
                "since": {
                    "type": ["string", "null"],
                    "description": "RFC-3339 timestamp cutoff; returns rows with created_at > since."
                },
                "limit": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "maximum": 1000,
                    "description": "Max rows; defaults to 50. Out-of-range values are rejected."
                },
                "author_session_id": {
                    "type": ["string", "null"],
                    "description": "Filter to one agent_sessions.id; null returns every author."
                }
            },
            "required": ["channel"],
            "additionalProperties": false
        }),
        output_schema: json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer" },
                    "epic_id": { "type": "string" },
                    "channel": { "type": "string" },
                    "irc_message_id": { "type": "string" },
                    "sender_actor_id": { "type": ["string", "null"] },
                    "command": { "type": "string" },
                    "raw_text": { "type": "string" },
                    "structured_event_id": { "type": ["string", "null"] },
                    "created_at": { "type": "string" }
                },
                "required": ["id", "epic_id", "channel", "irc_message_id",
                             "command", "raw_text", "created_at"]
            }
        }),
    }
}

/// Execute `chat.post`. Passthrough to [`chat_of_record::post_message`].
pub async fn post(pool: &SqlitePool, input: Value) -> Result<Value, McpToolError> {
    let parsed: ChatPostInput =
        serde_json::from_value(input).map_err(|source| McpToolError::InvalidInput {
            tool: POST_NAME,
            source,
        })?;

    let channel_echo = parsed.channel.clone();
    let id = chat_of_record::post_message(
        pool,
        NewPost {
            channel: parsed.channel,
            body: parsed.body,
            author_session_id: parsed.author_session_id,
            epic_id: parsed.epic_id,
        },
    )
    .await?;

    let output = ChatPostOutput {
        id,
        channel: channel_echo,
    };
    serde_json::to_value(&output).map_err(|source| McpToolError::OutputSerialize {
        tool: POST_NAME,
        source,
    })
}

/// Execute `chat.tail`. Passthrough to [`chat_of_record::tail`].
///
/// The output is the raw `Vec<IrcMessage>` serialised to JSON — the
/// same shape `ryve channel tail --json` emits — so MCP clients and
/// CLI consumers share a deserialiser.
pub async fn tail(pool: &SqlitePool, input: Value) -> Result<Value, McpToolError> {
    let parsed: ChatTailInput =
        serde_json::from_value(input).map_err(|source| McpToolError::InvalidInput {
            tool: TAIL_NAME,
            source,
        })?;

    let filter = TailFilter::for_channel(parsed.channel)
        .with_since(parsed.since)
        .with_limit(parsed.limit.unwrap_or(TAIL_DEFAULT_LIMIT))
        .with_author(parsed.author_session_id);

    let rows: Vec<IrcMessage> = chat_of_record::tail(pool, filter).await?;
    serde_json::to_value(&rows).map_err(|source| McpToolError::OutputSerialize {
        tool: TAIL_NAME,
        source,
    })
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;

    const EPIC_TITLE: &str = "MCP chat tools";

    async fn seed_epic(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO sparks \
             (id, title, description, status, priority, spark_type, workshop_id, created_at, updated_at) \
             VALUES (?, ?, '', 'open', 1, 'epic', 'ws-mcp', \
                     '2026-04-20T00:00:00Z', '2026-04-20T00:00:00Z')",
        )
        .bind(id)
        .bind(EPIC_TITLE)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_session(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO agent_sessions \
             (id, workshop_id, agent_name, agent_command, agent_args, status, started_at) \
             VALUES (?, 'ws-mcp', 'test', '/bin/true', '[]', 'running', \
                     '2026-04-20T00:00:00Z')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    fn channel_for(epic_id: &str) -> String {
        ipc::channel_manager::channel_name(&ipc::channel_manager::EpicRef {
            id: epic_id.to_string(),
            name: EPIC_TITLE.to_string(),
        })
    }

    #[sqlx::test(migrations = "data/migrations")]
    async fn chat_post_writes_irc_messages_row(pool: SqlitePool) {
        seed_epic(&pool, "mcp-1").await;
        let channel = channel_for("mcp-1");

        let result = post(
            &pool,
            json!({
                "channel": channel,
                "body": "plan: ship MCP tool wrappers",
            }),
        )
        .await
        .expect("chat.post should succeed");

        // Output shape matches the CLI's `ryve post --json` payload.
        let id = result
            .get("id")
            .and_then(|v| v.as_i64())
            .expect("id in output");
        assert!(id > 0, "id must be positive row key, got {id}");
        assert_eq!(
            result.get("channel").and_then(|v| v.as_str()),
            Some(channel.as_str())
        );

        // Direct DB read confirms the row landed, with the expected
        // defaults (command PRIVMSG, no sender, non-empty wire id).
        let (row_id, raw_text, command, sender_actor_id, epic_id, irc_message_id): (
            i64,
            String,
            String,
            Option<String>,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT id, raw_text, command, sender_actor_id, epic_id, irc_message_id \
             FROM irc_messages WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("row should exist after chat.post");
        assert_eq!(row_id, id);
        assert_eq!(raw_text, "plan: ship MCP tool wrappers");
        assert_eq!(command, "PRIVMSG");
        assert!(sender_actor_id.is_none());
        assert_eq!(epic_id, "mcp-1");
        assert!(!irc_message_id.is_empty());
    }

    #[sqlx::test(migrations = "data/migrations")]
    async fn chat_tail_returns_filtered_rows_matching_cli_shape(pool: SqlitePool) {
        seed_epic(&pool, "mcp-2").await;
        seed_session(&pool, "sess-alice").await;
        seed_session(&pool, "sess-bob").await;
        let channel = channel_for("mcp-2");

        // Seed three posts with strictly increasing created_at so
        // since/limit/author filters are deterministic.
        post(
            &pool,
            json!({ "channel": channel, "body": "alice 1",
                    "author_session_id": "sess-alice" }),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        post(
            &pool,
            json!({ "channel": channel, "body": "bob 1",
                    "author_session_id": "sess-bob" }),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        post(
            &pool,
            json!({ "channel": channel, "body": "alice 2",
                    "author_session_id": "sess-alice" }),
        )
        .await
        .unwrap();

        // Full tail — returns array of IrcMessage in chronological
        // order. This is the exact shape the CLI emits with --json.
        let all = tail(&pool, json!({ "channel": channel })).await.unwrap();
        let all = all.as_array().expect("chat.tail output must be array");
        assert_eq!(all.len(), 3);
        assert_eq!(
            all[0].get("raw_text").and_then(|v| v.as_str()),
            Some("alice 1")
        );
        assert_eq!(
            all[1].get("raw_text").and_then(|v| v.as_str()),
            Some("bob 1")
        );
        assert_eq!(
            all[2].get("raw_text").and_then(|v| v.as_str()),
            Some("alice 2")
        );
        // Shape parity: every row carries the full IrcMessage schema.
        for row in all {
            for field in [
                "id",
                "epic_id",
                "channel",
                "irc_message_id",
                "command",
                "raw_text",
                "created_at",
            ] {
                assert!(
                    row.get(field).is_some(),
                    "missing field {field} in chat.tail row: {row}"
                );
            }
        }

        // Author filter narrows to one session.
        let alice_only = tail(
            &pool,
            json!({ "channel": channel, "author_session_id": "sess-alice" }),
        )
        .await
        .unwrap();
        let alice_only = alice_only.as_array().unwrap();
        assert_eq!(alice_only.len(), 2);
        assert_eq!(
            alice_only[0].get("raw_text").and_then(|v| v.as_str()),
            Some("alice 1")
        );
        assert_eq!(
            alice_only[1].get("raw_text").and_then(|v| v.as_str()),
            Some("alice 2")
        );

        // Limit caps the returned count — matches CLI --limit semantics.
        let capped = tail(&pool, json!({ "channel": channel, "limit": 1 }))
            .await
            .unwrap();
        assert_eq!(capped.as_array().unwrap().len(), 1);
    }

    #[sqlx::test(migrations = "data/migrations")]
    async fn chat_post_rejects_unknown_fields(pool: SqlitePool) {
        // deny_unknown_fields — typos like `session_id` become errors
        // instead of silently-ignored fields that drift away from the
        // schema the descriptor advertises.
        let err = post(
            &pool,
            json!({
                "channel": "#epic-1-x",
                "body": "hi",
                "session_id": "sess-typo"
            }),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                McpToolError::InvalidInput {
                    tool: "chat.post",
                    ..
                }
            ),
            "expected InvalidInput, got {err:?}"
        );
    }

    #[sqlx::test(migrations = "data/migrations")]
    async fn chat_post_missing_required_field_is_invalid(pool: SqlitePool) {
        let err = post(&pool, json!({ "channel": "#epic-1-x" }))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                McpToolError::InvalidInput {
                    tool: "chat.post",
                    ..
                }
            ),
            "expected InvalidInput, got {err:?}"
        );
    }

    #[sqlx::test(migrations = "data/migrations")]
    async fn chat_tail_forwards_invalid_limit_from_primitive(pool: SqlitePool) {
        // Out-of-range limit is rejected by the wrapped primitive —
        // the MCP layer passes the typed error through unchanged so
        // the caller sees exactly the same surface the CLI does.
        let err = tail(&pool, json!({ "channel": "#epic-1-x", "limit": 0 }))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                McpToolError::Chat(ipc::chat_of_record::ChatError::InvalidLimit { got: 0, .. })
            ),
            "expected ChatError::InvalidLimit, got {err:?}"
        );
    }

    #[sqlx::test(migrations = "data/migrations")]
    async fn chat_tail_empty_channel_returns_empty_array(pool: SqlitePool) {
        let rows = tail(&pool, json!({ "channel": "#epic-999-unused" }))
            .await
            .unwrap();
        assert_eq!(rows.as_array().map(|a| a.len()), Some(0));
    }

    #[sqlx::test(migrations = "data/migrations")]
    async fn dispatcher_routes_chat_post_and_tail(pool: SqlitePool) {
        seed_epic(&pool, "mcp-3").await;
        let channel = channel_for("mcp-3");

        let post_out = super::super::call_tool(
            &pool,
            POST_NAME,
            json!({ "channel": channel, "body": "via dispatcher" }),
        )
        .await
        .unwrap();
        assert!(post_out.get("id").and_then(|v| v.as_i64()).unwrap() > 0);

        let tail_out = super::super::call_tool(&pool, TAIL_NAME, json!({ "channel": channel }))
            .await
            .unwrap();
        let arr = tail_out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0].get("raw_text").and_then(|v| v.as_str()),
            Some("via dispatcher")
        );
    }
}
