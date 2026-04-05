// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! LLM client wrapping the genai crate for multi-provider chat.

use genai::Client;
use genai::chat::{ChatMessage, ChatRequest};

use crate::proto::{Agent, Message, Role};

/// Ryve LLM client — thin wrapper over genai.
pub struct RyveClient {
    inner: Client,
}

impl RyveClient {
    pub fn new() -> Result<Self, ClientError> {
        let inner = Client::default();
        Ok(Self { inner })
    }

    /// Send a list of messages to the given agent's model and return the response.
    pub async fn chat(&self, agent: &Agent, messages: &[Message]) -> Result<String, ClientError> {
        let model = &agent.model;

        let mut request = ChatRequest::default();

        if let Some(ref system) = agent.system_prompt {
            request = request.with_system(system.as_str());
        }

        for msg in messages {
            let chat_msg = match msg.role {
                Role::User => ChatMessage::user(msg.content.as_str()),
                Role::Assistant => ChatMessage::assistant(msg.content.as_str()),
                Role::System => ChatMessage::system(msg.content.as_str()),
            };
            request = request.append_message(chat_msg);
        }

        let response = self
            .inner
            .exec_chat(model, request, None)
            .await
            .map_err(ClientError::Genai)?;

        response.into_first_text().ok_or(ClientError::EmptyResponse)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("genai error: {0}")]
    Genai(#[source] genai::Error),
    #[error("empty response from model")]
    EmptyResponse,
}
