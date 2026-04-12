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
    AlreadyClaimed {
        spark_id: String,
        session_id: String,
    },

    #[error("spark {spark_id} has failing required contract {contract_id}")]
    ContractViolation { spark_id: String, contract_id: i64 },

    #[error("spark {spark_id} has stale claim from session {session_id}")]
    StaleClaim {
        spark_id: String,
        session_id: String,
    },

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error(
        "non-epic spark must have a parent_id (spark_type={spark_type}); \
         only epics may be top-level. Pick a parent epic or create one first."
    )]
    OrphanChildRejected { spark_type: String },

    #[error("invalid semver: {0}")]
    InvalidSemver(String),

    #[error("spark {spark_id} already belongs to another open release")]
    EpicAlreadyInOpenRelease { spark_id: String },

    #[error("{0}")]
    Transition(#[from] TransitionError),
}

/// Errors specific to `assignment_phase` transition validation.
#[derive(Debug, thiserror::Error)]
pub enum TransitionError {
    #[error(
        "illegal transition from {from} to {to}: \
         not in the legal transition map"
    )]
    IllegalTransition {
        from: &'static str,
        to: &'static str,
    },

    #[error(
        "phase mismatch: expected {expected} but assignment is currently {actual} \
         (out-of-order replay)"
    )]
    PhaseMismatch {
        expected: &'static str,
        actual: &'static str,
    },

    #[error(
        "role {role} is not authorized for transition {from} → {to}; \
         authorized roles: {authorized}"
    )]
    Unauthorized {
        role: &'static str,
        from: &'static str,
        to: &'static str,
        authorized: String,
    },

    #[error("assignment {assignment_id} not found")]
    AssignmentNotFound { assignment_id: i64 },

    #[error("database error during transition: {0}")]
    Database(#[from] sqlx::Error),
}
