// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

#[derive(Debug, thiserror::Error)]
pub enum SparksError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("cycle detected: bond from {from} to {to} would create a cycle")]
    CycleDetected { from: String, to: String },

    #[error("GitHub sync error: {0}")]
    GitHubSync(String),

    #[error("invalid status: {0}")]
    InvalidStatus(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("spark {spark_id} is already claimed by session {session_id}")]
    AlreadyClaimed { spark_id: String, session_id: String },

    #[error("spark {spark_id} has failing required contract {contract_id}")]
    ContractViolation { spark_id: String, contract_id: i64 },

    #[error("spark {spark_id} has stale claim from session {session_id}")]
    StaleClaim { spark_id: String, session_id: String },

    #[error("serialization error: {0}")]
    Serialization(String),
}
