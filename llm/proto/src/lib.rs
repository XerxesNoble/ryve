// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Protocol types for Ryve LLM interactions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A conversation thread between a user and an LLM agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
}

/// A single message within a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub role: Role,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

/// Message author role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// An agent definition — an LLM persona attached to a worktree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: Uuid,
    pub name: String,
    pub provider: String,
    pub model: String,
    pub system_prompt: Option<String>,
    pub worktree_path: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Thread {
    pub fn new(agent_id: Uuid, title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            agent_id,
            title: title.into(),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        }
    }
}

impl Message {
    pub fn new(thread_id: Uuid, role: Role, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            thread_id,
            role,
            content: content.into(),
            created_at: Utc::now(),
        }
    }
}

impl Agent {
    pub fn new(
        name: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            provider: provider.into(),
            model: model.into(),
            system_prompt: None,
            worktree_path: None,
            created_at: Utc::now(),
        }
    }
}
