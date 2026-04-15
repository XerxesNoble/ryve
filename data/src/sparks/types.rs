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

    #[allow(clippy::should_implement_trait)]
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
    /// `agent_sessions.id` of the Hand that spawned this one — typically a
    /// Head when a child Hand is dispatched via `ryve hand spawn`. `None`
    /// for sessions started directly by the user from the UI. Used by the
    /// Hands panel to attribute solo Hands to their parent Head when the
    /// child is not in any of the Head's crews.
    pub parent_session_id: Option<String>,
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
    pub parent_session_id: Option<String>,
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

// ── Assignment ───────────────────────────────────────

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
    pub phase: String,
    pub event_version: i64,
    pub source_branch: Option<String>,
    pub target_branch: Option<String>,
    pub assigned_at: String,
    pub last_heartbeat_at: Option<String>,
    pub lease_expires_at: Option<String>,
    pub completed_at: Option<String>,
    pub handoff_to: Option<String>,
    pub handoff_reason: Option<String>,
    /// Current workflow phase, governed by the transition validator.
    /// `None` for rows created before the migration adds this column.
    pub assignment_phase: Option<String>,
    /// When the phase was last changed (RFC 3339).
    pub phase_changed_at: Option<String>,
    /// actor_id that performed the last phase transition.
    pub phase_changed_by: Option<String>,
    /// Role of the actor that performed the last phase transition.
    pub phase_actor_role: Option<String>,
    /// Event ID that triggered the last phase transition.
    pub phase_event_id: Option<i64>,
}

pub struct NewHandAssignment {
    pub session_id: String,
    pub spark_id: String,
    pub role: AssignmentRole,
    pub source_branch: Option<String>,
    pub target_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Assignment {
    pub assignment_id: String,
    pub spark_id: String,
    pub actor_id: String,
    pub assignment_phase: String,
    pub source_branch: Option<String>,
    pub target_branch: Option<String>,
    pub event_version: i64,
    pub created_at: String,
    pub updated_at: String,
}

pub struct NewAssignment {
    pub spark_id: String,
    pub actor_id: String,
    pub assignment_phase: AssignmentPhase,
    pub source_branch: Option<String>,
    pub target_branch: Option<String>,
}

pub struct UpdateAssignment {
    pub event_version: Option<i64>,
    pub source_branch: Option<Option<String>>,
    pub target_branch: Option<Option<String>>,
}

// ── Assignment Phase (transition state machine) ─────

/// The workflow phase of an assignment, governed by a strict transition
/// validator. Only the `transition::transition_assignment_phase` function
/// may advance this value — direct UPDATEs are forbidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentPhase {
    Assigned,
    InProgress,
    AwaitingReview,
    Approved,
    Rejected,
    InRepair,
    ReadyForMerge,
    Merged,
}

impl AssignmentPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Assigned => "assigned",
            Self::InProgress => "in_progress",
            Self::AwaitingReview => "awaiting_review",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::InRepair => "in_repair",
            Self::ReadyForMerge => "ready_for_merge",
            Self::Merged => "merged",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "assigned" => Some(Self::Assigned),
            "in_progress" => Some(Self::InProgress),
            "awaiting_review" => Some(Self::AwaitingReview),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "in_repair" => Some(Self::InRepair),
            "ready_for_merge" => Some(Self::ReadyForMerge),
            "merged" => Some(Self::Merged),
            _ => None,
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::Assigned,
        Self::InProgress,
        Self::AwaitingReview,
        Self::Approved,
        Self::Rejected,
        Self::InRepair,
        Self::ReadyForMerge,
        Self::Merged,
    ];
}

/// Role of the actor performing a phase transition. This is distinct from
/// `AgentRole` (Director/Head/Hand hierarchy) and `AssignmentRole`
/// (Owner/Assistant/Observer/Merger relationship to a spark). Transition
/// actor roles encode *who is allowed to advance the state machine*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionActorRole {
    /// A Hand doing implementation work on the spark.
    Hand,
    /// A Hand performing code review.
    ReviewerHand,
    /// A Hand performing merge/integration.
    MergeHand,
    /// A Head orchestrating the crew — may override any transition.
    Head,
    /// The top-level Director — may override any transition.
    Director,
}

impl TransitionActorRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hand => "hand",
            Self::ReviewerHand => "reviewer_hand",
            Self::MergeHand => "merge_hand",
            Self::Head => "head",
            Self::Director => "director",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "hand" => Some(Self::Hand),
            "reviewer_hand" => Some(Self::ReviewerHand),
            "merge_hand" => Some(Self::MergeHand),
            "head" => Some(Self::Head),
            "director" => Some(Self::Director),
            _ => None,
        }
    }

    /// Returns `true` if this role can override any transition regardless
    /// of the normal role-ownership rules.
    pub fn can_override(&self) -> bool {
        matches!(self, Self::Head | Self::Director)
    }
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

// ── Agent Role Model ─────────────────────────────────
//
// Ryve's agent system has three first-class roles. They form a strict
// delegation hierarchy: a Director plans and routes work, Heads
// orchestrate Crews, Hands execute sparks. The role is intrinsic to the
// agent — it determines which prompts, tools, and authority the agent
// gets — and is distinct from `AssignmentRole` (which describes how a
// specific Hand session relates to a single spark).
//
// Spark `ryve-d772adfe`: prior to this enum the hierarchy lived only in
// docs and prompt strings; "Director" wasn't represented in code at all,
// so there was no place to anchor a default user-facing agent. Atlas
// (see [`Agent::atlas`]) is that anchor: Ryve's primary Director.

/// First-class agent role in Ryve's Director → Head → Hand hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Top of the hierarchy: the user-facing planner. Receives requests
    /// directly from the user, decomposes them into work, and delegates
    /// to Heads (or, for trivial tasks, straight to a Hand). A workshop
    /// has exactly one default Director — Atlas.
    Director,
    /// Mid-tier orchestrator: owns a Crew of Hands, splits a goal into
    /// sparks, dispatches them, and integrates the results. A Director
    /// can spawn many Heads in parallel for unrelated initiatives.
    Head,
    /// Leaf executor: claims a single spark, does the work in an
    /// isolated worktree, and closes the spark. Reports up to its Head
    /// (or directly to the Director if spawned solo).
    Hand,
}

impl AgentRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Director => "director",
            Self::Head => "head",
            Self::Hand => "hand",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "director" => Some(Self::Director),
            "head" => Some(Self::Head),
            "hand" => Some(Self::Hand),
            _ => None,
        }
    }

    /// True if this role may delegate work to the `other` role.
    /// Encodes the strict Director → Head → Hand hierarchy.
    pub fn can_delegate_to(&self, other: AgentRole) -> bool {
        matches!(
            (self, other),
            (Self::Director, Self::Head) | (Self::Director, Self::Hand) | (Self::Head, Self::Hand)
        )
    }
}

/// A named agent identity within Ryve, paired with its role. This is the
/// in-memory representation of a "who" — Atlas the Director, an
/// anonymous Head spawned by the user, etc. Persistence still happens
/// through `agent_sessions`; this type lets the orchestration layer
/// reason about role and identity without parsing free-form labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub role: AgentRole,
}

impl Agent {
    pub fn new(name: impl Into<String>, role: AgentRole) -> Self {
        Self {
            name: name.into(),
            role,
        }
    }

    /// Atlas — Ryve's default, primary user-facing Director. Every
    /// workshop instantiates Atlas at boot so the user always has a
    /// stable conversational counterpart, even before any Heads or
    /// Hands have been spawned. See parent epic `ryve-5472d4c6`.
    pub fn atlas() -> Self {
        Self {
            name: ATLAS_NAME.to_string(),
            role: AgentRole::Director,
        }
    }
}

/// Canonical display name for the default Director. Kept as a constant
/// so UI copy, prompts, and logs all reference the same string.
pub const ATLAS_NAME: &str = "Atlas";

// ── Delegation Trace ─────────────────────────────────

/// The Director identity that owns originating requests by default. Atlas is
/// Ryve's primary user-facing agent, so every trace whose origin is not
/// explicitly overridden is recorded as Atlas-originated. Spark
/// ryve-1e3848b6.
pub const ATLAS_ORIGIN: &str = "atlas";

/// Where a participant in a delegation hop sits in the agent hierarchy. Used
/// for both the delegating actor and the delegated target so a hop can
/// describe e.g. `Director → Head` or `Head → Hand`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// The top-level Director agent (Atlas).
    Director,
    /// A Head: orchestrator that manages a Crew of Hands.
    Head,
    /// A Hand: leaf worker that runs against a single spark.
    Hand,
    /// A non-agent tool invocation (shell, MCP server, etc.).
    Tool,
    /// The human user — only ever appears as the delegating actor on the root
    /// hop when the user talks directly to a non-Director agent.
    User,
}

impl ActorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Director => "director",
            Self::Head => "head",
            Self::Hand => "hand",
            Self::Tool => "tool",
            Self::User => "user",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "director" => Some(Self::Director),
            "head" => Some(Self::Head),
            "hand" => Some(Self::Hand),
            "tool" => Some(Self::Tool),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl DelegationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One hop in a delegation chain. The chain is reconstructed by walking
/// `parent_trace_id` upward; the root hop's `origin_actor` (typically
/// `ATLAS_ORIGIN`) identifies the Director that owns the entire request.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DelegationTrace {
    pub id: String,
    pub workshop_id: String,
    pub spark_id: Option<String>,
    pub parent_trace_id: Option<String>,
    pub originating_request: String,
    pub origin_actor: String,
    pub delegating_actor: String,
    pub delegating_actor_kind: String,
    pub delegated_target: String,
    pub delegated_target_kind: String,
    pub status: String,
    pub execution_result: Option<String>,
    pub final_synthesis: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

impl DelegationTrace {
    /// True when this trace was rooted at Atlas (the default Director).
    /// Used by the UI and tests to assert Atlas visibility on a trace.
    pub fn is_atlas_originated(&self) -> bool {
        self.origin_actor == ATLAS_ORIGIN
    }
}

pub struct NewDelegationTrace {
    pub workshop_id: String,
    pub spark_id: Option<String>,
    pub parent_trace_id: Option<String>,
    pub originating_request: String,
    /// Director identity that owns the request. `None` defaults to Atlas.
    pub origin_actor: Option<String>,
    pub delegating_actor: String,
    pub delegating_actor_kind: ActorKind,
    pub delegated_target: String,
    pub delegated_target_kind: ActorKind,
}

// ── Release ────────────────────────────────────────────

/// A release bundles one or more epic sparks into a shippable unit with its
/// own lifecycle. See migration `011_releases.sql` and `release_repo`.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Release {
    pub id: String,
    pub version: String,
    pub status: String,
    pub branch_name: Option<String>,
    pub created_at: String,
    pub cut_at: Option<String>,
    pub tag: Option<String>,
    pub artifact_path: Option<String>,
    pub problem: Option<String>,
    pub acceptance_json: String,
    pub notes: Option<String>,
}

impl Release {
    /// Parse the stored `acceptance_json` blob into a vector of criteria.
    /// Returns an empty vec on any parse error — callers that care should
    /// validate on write before persistence, as `release_repo::create`
    /// serializes `NewRelease.acceptance` directly.
    pub fn acceptance(&self) -> Vec<String> {
        serde_json::from_str(&self.acceptance_json).unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct NewRelease {
    pub version: String,
    pub branch_name: Option<String>,
    pub problem: Option<String>,
    pub acceptance: Vec<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStatus {
    Planning,
    InProgress,
    Ready,
    Cut,
    Closed,
    Abandoned,
}

impl ReleaseStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::InProgress => "in_progress",
            Self::Ready => "ready",
            Self::Cut => "cut",
            Self::Closed => "closed",
            Self::Abandoned => "abandoned",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "planning" => Some(Self::Planning),
            "in_progress" => Some(Self::InProgress),
            "ready" => Some(Self::Ready),
            "cut" => Some(Self::Cut),
            "closed" => Some(Self::Closed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    /// An "open" release is one that still blocks other releases from
    /// claiming the same epic. Matches the invariant enforced by the
    /// `release_epics_single_open_insert` trigger.
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Planning | Self::InProgress | Self::Ready)
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct ReleaseEpic {
    pub release_id: String,
    pub spark_id: String,
    pub added_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spark `ryve-d772adfe`: Director, Head, and Hand must all be
    /// first-class enum variants with stable string representations so
    /// the rest of the orchestration layer can pattern-match on them
    /// instead of stringly-typed agent labels.
    #[test]
    fn agent_role_has_director_head_and_hand_variants() {
        assert_eq!(AgentRole::Director.as_str(), "director");
        assert_eq!(AgentRole::Head.as_str(), "head");
        assert_eq!(AgentRole::Hand.as_str(), "hand");

        assert_eq!(AgentRole::from_str("director"), Some(AgentRole::Director));
        assert_eq!(AgentRole::from_str("head"), Some(AgentRole::Head));
        assert_eq!(AgentRole::from_str("hand"), Some(AgentRole::Hand));
        assert_eq!(AgentRole::from_str("merger"), None);
    }

    /// The role hierarchy is strict: a Director can delegate down to
    /// Heads and (in trivial cases) Hands; a Head can dispatch to
    /// Hands; nothing delegates upward and Hands do not delegate at
    /// all. Encodes the invariant from the parent epic.
    #[test]
    fn agent_role_delegation_follows_hierarchy() {
        assert!(AgentRole::Director.can_delegate_to(AgentRole::Head));
        assert!(AgentRole::Director.can_delegate_to(AgentRole::Hand));
        assert!(AgentRole::Head.can_delegate_to(AgentRole::Hand));

        // No upward delegation.
        assert!(!AgentRole::Hand.can_delegate_to(AgentRole::Head));
        assert!(!AgentRole::Hand.can_delegate_to(AgentRole::Director));
        assert!(!AgentRole::Head.can_delegate_to(AgentRole::Director));

        // Same-level / self delegation is not delegation.
        assert!(!AgentRole::Director.can_delegate_to(AgentRole::Director));
        assert!(!AgentRole::Head.can_delegate_to(AgentRole::Head));
        assert!(!AgentRole::Hand.can_delegate_to(AgentRole::Hand));
    }

    /// Acceptance criterion for `ryve-d772adfe`: Atlas is instantiated
    /// as the default Director — same name, same role, every time.
    #[test]
    fn atlas_is_the_default_director() {
        let atlas = Agent::atlas();
        assert_eq!(atlas.name, ATLAS_NAME);
        assert_eq!(atlas.name, "Atlas");
        assert_eq!(atlas.role, AgentRole::Director);
    }

    #[test]
    fn agent_role_round_trips_through_serde() {
        for role in [AgentRole::Director, AgentRole::Head, AgentRole::Hand] {
            let json = serde_json::to_string(&role).unwrap();
            let back: AgentRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, back, "round-trip mismatch for {role:?}");
        }
    }

    #[test]
    fn assignment_phase_as_str_round_trips() {
        for phase in AssignmentPhase::ALL {
            let s = phase.as_str();
            let back =
                AssignmentPhase::from_str(s).unwrap_or_else(|| panic!("from_str failed for {s:?}"));
            assert_eq!(*phase, back);
        }
    }

    #[test]
    fn assignment_phase_serde_round_trips() {
        for phase in AssignmentPhase::ALL {
            let json = serde_json::to_string(phase).unwrap();
            let back: AssignmentPhase = serde_json::from_str(&json).unwrap();
            assert_eq!(*phase, back, "serde round-trip mismatch for {phase:?}");
        }
    }

    #[test]
    fn assignment_phase_all_has_expected_count() {
        assert_eq!(AssignmentPhase::ALL.len(), 8);
    }
}
