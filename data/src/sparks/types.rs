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
    pub risk_level: Option<String>,
    pub scope_boundary: Option<String>,
}

impl Spark {
    /// Parse structured intent from the metadata JSON `"intent"` key.
    pub fn intent(&self) -> SparkIntent {
        serde_json::from_str::<serde_json::Value>(&self.metadata)
            .ok()
            .and_then(|v| serde_json::from_value(v["intent"].clone()).ok())
            .unwrap_or_default()
    }
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
    pub risk_level: Option<RiskLevel>,
    pub scope_boundary: Option<String>,
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
    pub risk_level: Option<RiskLevel>,
    pub scope_boundary: Option<Option<String>>,
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
    pub risk_level: Option<RiskLevel>,
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
    pub actor_type: Option<String>,
    pub change_nature: Option<String>,
    pub session_id: Option<String>,
}

pub struct NewEvent {
    pub spark_id: String,
    pub actor: String,
    pub field_name: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub reason: Option<String>,
    pub actor_type: Option<ActorType>,
    pub change_nature: Option<ChangeNature>,
    pub session_id: Option<String>,
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
    pub child_pid: Option<i64>,
    /// Agent-specific session/conversation ID used for resumption.
    pub resume_id: Option<String>,
    /// Filesystem path to the detached child's stdout/stderr log file
    /// (set for CLI-spawned background Hands; `None` for UI-spawned
    /// sessions whose output flows through their `iced_term` tab).
    pub log_path: Option<String>,
}

pub struct NewAgentSession {
    pub id: String,
    pub workshop_id: String,
    pub agent_name: String,
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub session_label: Option<String>,
    pub child_pid: Option<i64>,
    pub resume_id: Option<String>,
    pub log_path: Option<String>,
}

// ── Structured Intent ────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Trivial,
    Normal,
    Elevated,
    Critical,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Trivial => "trivial",
            Self::Normal => "normal",
            Self::Elevated => "elevated",
            Self::Critical => "critical",
        }
    }
}

/// Structured intent embedded in spark metadata JSON under the `"intent"` key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SparkIntent {
    #[serde(default)]
    pub problem_statement: Option<String>,
    #[serde(default)]
    pub invariants: Vec<String>,
    #[serde(default)]
    pub non_goals: Vec<String>,
    #[serde(default)]
    pub verification_summary: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

// ── Verification Contract ────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractKind {
    TestPass,
    NoApiBreak,
    CustomCommand,
    GrepAbsent,
    GrepPresent,
}

impl ContractKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TestPass => "test_pass",
            Self::NoApiBreak => "no_api_break",
            Self::CustomCommand => "custom_command",
            Self::GrepAbsent => "grep_absent",
            Self::GrepPresent => "grep_present",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    Pending,
    Pass,
    Fail,
    Skipped,
}

impl ContractStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractEnforcement {
    Advisory,
    Required,
}

impl ContractEnforcement {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Contract {
    pub id: i64,
    pub spark_id: String,
    pub kind: String,
    pub description: String,
    pub check_command: Option<String>,
    pub pattern: Option<String>,
    pub file_glob: Option<String>,
    pub enforcement: String,
    pub status: String,
    pub last_checked_at: Option<String>,
    pub last_checked_by: Option<String>,
    pub created_at: String,
}

pub struct NewContract {
    pub spark_id: String,
    pub kind: ContractKind,
    pub description: String,
    pub check_command: Option<String>,
    pub pattern: Option<String>,
    pub file_glob: Option<String>,
    pub enforcement: ContractEnforcement,
}

// ── Provenance ───────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorType {
    Human,
    Hand,
    System,
    Unknown,
}

impl ActorType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Hand => "hand",
            Self::System => "system",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeNature {
    Code,
    Refactor,
    Format,
    Generated,
    Review,
    Config,
    Documentation,
    Test,
}

impl ChangeNature {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Refactor => "refactor",
            Self::Format => "format",
            Self::Generated => "generated",
            Self::Review => "review",
            Self::Config => "config",
            Self::Documentation => "documentation",
            Self::Test => "test",
        }
    }
}

// ── Commit Link ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CommitLink {
    pub id: i64,
    pub spark_id: String,
    pub commit_hash: String,
    pub commit_message: Option<String>,
    pub author: Option<String>,
    pub committed_at: Option<String>,
    pub workshop_id: String,
    pub linked_by: String,
    pub created_at: String,
}

pub struct NewCommitLink {
    pub spark_id: String,
    pub commit_hash: String,
    pub commit_message: Option<String>,
    pub author: Option<String>,
    pub committed_at: Option<String>,
    pub workshop_id: String,
    pub linked_by: String,
}

// ── Architectural Constraint ─────────────────────────

/// Stored as JSON value in an engraving with key prefix `constraint:`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchConstraint {
    pub rule: String,
    pub kind: ConstraintKind,
    #[serde(default)]
    pub check: Option<ConstraintCheck>,
    pub severity: ConstraintSeverity,
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    ImportBoundary,
    DataFlow,
    NamingConvention,
    SecurityPolicy,
    PerformanceBudget,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstraintCheck {
    #[serde(rename = "type")]
    pub check_type: String,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub glob: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintSeverity {
    Error,
    Warning,
    Info,
}

// ── Hand Assignment ──────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentStatus {
    Active,
    Completed,
    HandedOff,
    Abandoned,
    Expired,
}

impl AssignmentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::HandedOff => "handed_off",
            Self::Abandoned => "abandoned",
            Self::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentRole {
    Owner,
    Assistant,
    Observer,
    /// A Hand whose job is to integrate the Crew's worktree branches into a
    /// single PR. Created by the Head when every other Crew member has
    /// closed its spark. See `compose_merger_prompt` in `agent_prompts.rs`.
    Merger,
}

impl AssignmentRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Assistant => "assistant",
            Self::Observer => "observer",
            Self::Merger => "merger",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(Self::Owner),
            "assistant" => Some(Self::Assistant),
            "observer" => Some(Self::Observer),
            "merger" => Some(Self::Merger),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct HandAssignment {
    pub id: i64,
    pub session_id: String,
    pub spark_id: String,
    pub status: String,
    pub role: String,
    pub assigned_at: String,
    pub last_heartbeat_at: Option<String>,
    pub lease_expires_at: Option<String>,
    pub completed_at: Option<String>,
    pub handoff_to: Option<String>,
    pub handoff_reason: Option<String>,
}

pub struct NewHandAssignment {
    pub session_id: String,
    pub spark_id: String,
    pub role: AssignmentRole,
}

// ── Crew ──────────────────────────────────────────────

/// A Crew is a group of Hands working in parallel on related sparks under
/// the direction of a Head. The Head and the (eventual) Merger are recorded
/// here so the workgraph remains the single source of truth for who is
/// orchestrating whom.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Crew {
    pub id: String,
    pub workshop_id: String,
    pub name: String,
    pub purpose: Option<String>,
    pub status: String,
    pub head_session_id: Option<String>,
    pub parent_spark_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct NewCrew {
    pub name: String,
    pub purpose: Option<String>,
    pub workshop_id: String,
    pub head_session_id: Option<String>,
    pub parent_spark_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewStatus {
    Active,
    Merging,
    Completed,
    Abandoned,
}

impl CrewStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Merging => "merging",
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "merging" => Some(Self::Merging),
            "completed" => Some(Self::Completed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CrewMember {
    pub id: i64,
    pub crew_id: String,
    pub session_id: String,
    pub role: Option<String>,
    pub joined_at: String,
}
