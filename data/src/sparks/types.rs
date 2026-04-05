// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Domain types for the Workgraph system.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

// ── Spark ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Spark {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: i32,
    pub spark_type: String,
    pub assignee: Option<String>,
    pub owner: Option<String>,
    pub parent_id: Option<String>,
    pub workshop_id: String,
    pub estimated_minutes: Option<i32>,
    pub github_issue_number: Option<i32>,
    pub github_repo: Option<String>,
    pub metadata: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub closed_reason: Option<String>,
    pub due_at: Option<String>,
    pub defer_until: Option<String>,
}

pub struct NewSpark {
    pub title: String,
    pub description: String,
    pub spark_type: SparkType,
    pub priority: i32,
    pub workshop_id: String,
    pub assignee: Option<String>,
    pub owner: Option<String>,
    pub parent_id: Option<String>,
    pub due_at: Option<String>,
    pub estimated_minutes: Option<i32>,
    pub metadata: Option<String>,
}

#[derive(Default)]
pub struct UpdateSpark {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<SparkStatus>,
    pub priority: Option<i32>,
    pub spark_type: Option<SparkType>,
    pub assignee: Option<Option<String>>,
    pub owner: Option<Option<String>>,
    pub parent_id: Option<Option<String>>,
    pub due_at: Option<Option<String>>,
    pub defer_until: Option<Option<String>>,
    pub estimated_minutes: Option<Option<i32>>,
    pub metadata: Option<String>,
}

#[derive(Default)]
pub struct SparkFilter {
    pub workshop_id: Option<String>,
    pub status: Option<Vec<SparkStatus>>,
    pub priority: Option<i32>,
    pub assignee: Option<String>,
    pub spark_type: Option<SparkType>,
    pub parent_id: Option<String>,
    pub stamp: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SparkStatus {
    Open,
    InProgress,
    Blocked,
    Deferred,
    Closed,
}

impl SparkStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Deferred => "deferred",
            Self::Closed => "closed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "in_progress" => Some(Self::InProgress),
            "blocked" => Some(Self::Blocked),
            "deferred" => Some(Self::Deferred),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SparkType {
    Bug,
    Feature,
    Task,
    Epic,
    Chore,
    Spike,
    Milestone,
}

impl SparkType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Feature => "feature",
            Self::Task => "task",
            Self::Epic => "epic",
            Self::Chore => "chore",
            Self::Spike => "spike",
            Self::Milestone => "milestone",
        }
    }
}

// ── Spark File Link ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SparkFileLink {
    pub id: i64,
    pub spark_id: String,
    pub file_path: String,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub workshop_id: String,
    pub created_at: String,
}

pub struct NewSparkFileLink {
    pub spark_id: String,
    pub file_path: String,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub workshop_id: String,
}

// ── Bond ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Bond {
    pub id: i64,
    pub from_id: String,
    pub to_id: String,
    pub bond_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BondType {
    Blocks,
    ParentChild,
    Related,
    ConditionalBlocks,
    WaitsFor,
    Duplicates,
    Supersedes,
}

impl BondType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::ParentChild => "parent_child",
            Self::Related => "related",
            Self::ConditionalBlocks => "conditional_blocks",
            Self::WaitsFor => "waits_for",
            Self::Duplicates => "duplicates",
            Self::Supersedes => "supersedes",
        }
    }

    pub fn is_blocking(&self) -> bool {
        matches!(self, Self::Blocks | Self::ConditionalBlocks)
    }
}

// ── Stamp ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Stamp {
    pub spark_id: String,
    pub name: String,
}

// ── Comment ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Comment {
    pub id: String,
    pub spark_id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

pub struct NewComment {
    pub spark_id: String,
    pub author: String,
    pub body: String,
}

// ── Event ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Event {
    pub id: i64,
    pub spark_id: String,
    pub actor: String,
    pub field_name: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub reason: Option<String>,
    pub timestamp: String,
}

pub struct NewEvent {
    pub spark_id: String,
    pub actor: String,
    pub field_name: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub reason: Option<String>,
}

// ── Ember ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Ember {
    pub id: String,
    pub ember_type: String,
    pub content: String,
    pub source_agent: Option<String>,
    pub workshop_id: String,
    pub ttl_seconds: i32,
    pub created_at: String,
}

pub struct NewEmber {
    pub ember_type: EmberType,
    pub content: String,
    pub source_agent: Option<String>,
    pub workshop_id: String,
    pub ttl_seconds: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmberType {
    Glow,
    Flash,
    Flare,
    Blaze,
    Ash,
}

impl EmberType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Glow => "glow",
            Self::Flash => "flash",
            Self::Flare => "flare",
            Self::Blaze => "blaze",
            Self::Ash => "ash",
        }
    }
}

// ── Engraving ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Engraving {
    pub key: String,
    pub workshop_id: String,
    pub value: String,
    pub author: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct NewEngraving {
    pub key: String,
    pub workshop_id: String,
    pub value: String,
    pub author: Option<String>,
}

// ── Alloy ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Alloy {
    pub id: String,
    pub name: String,
    pub alloy_type: String,
    pub parent_spark_id: Option<String>,
    pub workshop_id: String,
    pub created_at: String,
}

pub struct NewAlloy {
    pub name: String,
    pub alloy_type: AlloyType,
    pub parent_spark_id: Option<String>,
    pub workshop_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlloyType {
    Scatter,
    Watch,
    Chain,
}

impl AlloyType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Scatter => "scatter",
            Self::Watch => "watch",
            Self::Chain => "chain",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AlloyMember {
    pub alloy_id: String,
    pub spark_id: String,
    pub bond_type: String,
    pub position: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlloyBondType {
    Sequential,
    Parallel,
    Conditional,
}

impl AlloyBondType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Parallel => "parallel",
            Self::Conditional => "conditional",
        }
    }
}

// ── Agent Session ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PersistedAgentSession {
    pub id: String,
    pub workshop_id: String,
    pub agent_name: String,
    pub agent_command: String,
    pub agent_args: String,
    pub session_label: Option<String>,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// Agent-specific session/conversation ID used for resumption.
    pub resume_id: Option<String>,
}

pub struct NewAgentSession {
    pub id: String,
    pub workshop_id: String,
    pub agent_name: String,
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub session_label: Option<String>,
    pub resume_id: Option<String>,
}
