// SPDX-License-Identifier: AGPL-3.0-or-later

//! MCP tool server — registry + dispatcher.
//!
//! This module is the in-process Ryve MCP tool surface. It owns the
//! catalogue of tools an external MCP client (or the future stdio MCP
//! transport) can call, validates their JSON input against a typed
//! schema, and routes the call to the same code paths the CLI uses.
//!
//! ## Contract
//!
//! - Every tool is identified by a dotted `name` (e.g. `chat.post`).
//! - Inputs arrive as [`serde_json::Value`] and are deserialised into a
//!   tool-specific typed input struct. A deserialisation failure is
//!   reported as [`McpToolError::InvalidInput`] — the typed struct is
//!   the authoritative schema, and the explicit JSON Schema exposed via
//!   [`ToolDescriptor::input_schema`] mirrors it.
//! - Outputs are returned as [`serde_json::Value`] serialised from a
//!   typed output struct, matching the JSON shape the CLI's `--json`
//!   mode already emits.
//! - Semantic enforcement (e.g. channel→epic resolution, limit bounds)
//!   lives in the wrapped primitives — the MCP layer is a thin
//!   passthrough. DB write is the contract; IRC wire delivery stays
//!   best-effort.
//!
//! ## Registration
//!
//! New tools are added to [`all_tools`] and dispatched in [`call_tool`].
//! Keep both functions in sync: the descriptor list advertises the
//! surface, the dispatcher serves it.

pub mod chat_tools;

use serde_json::Value;
use sqlx::SqlitePool;
use thiserror::Error;

/// Errors returned by [`call_tool`] and individual tool handlers.
#[derive(Debug, Error)]
pub enum McpToolError {
    /// The caller asked for a tool name we do not register.
    #[error("unknown MCP tool: {0}")]
    UnknownTool(String),
    /// The caller's JSON input did not match the tool's typed schema
    /// (missing required field, wrong type, unknown field, etc.).
    #[error("invalid input for {tool}: {source}")]
    InvalidInput {
        tool: &'static str,
        #[source]
        source: serde_json::Error,
    },
    /// Serialising a tool's typed output back to [`Value`] failed. This
    /// is a bug in the tool, not the caller; exposed for completeness.
    #[error("output serialisation failed for {tool}: {source}")]
    OutputSerialize {
        tool: &'static str,
        #[source]
        source: serde_json::Error,
    },
    /// The underlying [`ipc::chat_of_record`] primitive failed. Kept as
    /// a typed variant so callers can distinguish semantic errors
    /// (unresolved epic, out-of-range limit) from transport errors.
    #[error("chat_of_record: {0}")]
    Chat(#[from] ipc::chat_of_record::ChatError),
}

/// Public metadata for one tool. The schemas are JSON Schema documents
/// (draft 2020-12 compatible) intended for MCP clients or for a future
/// introspection endpoint.
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub output_schema: Value,
}

/// Enumerate every tool registered with the MCP server.
///
/// Keep this in sync with [`call_tool`] — the descriptor advertises the
/// surface, the dispatcher serves it.
pub fn all_tools() -> Vec<ToolDescriptor> {
    vec![chat_tools::post_descriptor(), chat_tools::tail_descriptor()]
}

/// Dispatch an MCP tool call by name.
///
/// `input` is the raw JSON object from the MCP client; it is validated
/// by deserialising into the tool's typed input struct before any side
/// effects happen. The returned [`Value`] matches the tool's declared
/// `output_schema`.
pub async fn call_tool(pool: &SqlitePool, name: &str, input: Value) -> Result<Value, McpToolError> {
    match name {
        chat_tools::POST_NAME => chat_tools::post(pool, input).await,
        chat_tools::TAIL_NAME => chat_tools::tail(pool, input).await,
        other => Err(McpToolError::UnknownTool(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tools_lists_both_chat_tools() {
        let tools = all_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert!(
            names.contains(&"chat.post"),
            "expected chat.post in {names:?}"
        );
        assert!(
            names.contains(&"chat.tail"),
            "expected chat.tail in {names:?}"
        );
    }

    #[test]
    fn descriptors_carry_non_empty_schemas() {
        for t in all_tools() {
            assert!(
                t.input_schema.is_object(),
                "{} input_schema not object",
                t.name
            );
            assert!(
                t.output_schema.is_object() || t.output_schema.is_array(), /* array schema is object too */
                "{} output_schema malformed",
                t.name
            );
            assert!(!t.description.is_empty(), "{} description empty", t.name);
        }
    }

    #[tokio::test]
    async fn unknown_tool_is_reported() {
        // No pool is ever touched when the tool name is unknown, so we
        // can safely pass a dummy pool built from :memory: — but even
        // that is overkill. Cheat: construct a pool only when needed.
        // Here the dispatcher fails before consulting `pool`, so a
        // freshly-built in-memory pool is fine.
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let err = call_tool(&pool, "does.not.exist", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, McpToolError::UnknownTool(ref n) if n == "does.not.exist"));
    }
}
