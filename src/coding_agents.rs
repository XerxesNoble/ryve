// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Auto-detection of CLI coding agents available on the system.

use std::fmt;

/// A coding agent that can be launched from the bench.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgent {
    /// Display name shown in the dropdown (e.g. "Claude Code")
    pub display_name: String,
    /// CLI command to run (e.g. "claude")
    pub command: String,
    /// Default arguments
    pub args: Vec<String>,
    /// How this agent supports session resumption
    pub resume: ResumeStrategy,
}

impl CodingAgent {
    /// Build the command + args to resume a previous session.
    /// Returns None if the agent doesn't support resumption.
    pub fn resume_args(&self, resume_id: Option<&str>) -> Option<(String, Vec<String>)> {
        match &self.resume {
            ResumeStrategy::ResumeFlag => {
                // `claude --resume` or `codex --resume`
                let mut args = self.args.clone();
                args.push("--resume".to_string());
                if let Some(id) = resume_id {
                    args.push(id.to_string());
                }
                Some((self.command.clone(), args))
            }
            ResumeStrategy::SessionResume => {
                let id = resume_id?;
                // e.g., `goose session resume <id>`
                Some((self.command.clone(), vec!["session".into(), "resume".into(), id.to_string()]))
            }
            ResumeStrategy::None => None,
        }
    }
}

impl CodingAgent {
    /// Return the CLI flag for injecting a system prompt file, if supported.
    /// Returns `(flag, is_file_path)` — if `is_file_path` is false, the value
    /// should be inline text rather than a file path.
    pub fn system_prompt_flag(&self) -> Option<(&'static str, bool)> {
        match self.command.as_str() {
            "claude" => Some(("--system-prompt", true)),
            "codex" => Some(("--instructions", true)),
            "aider" => Some(("--read", true)),
            "opencode" => Some(("--prompt", false)),
            _ => None,
        }
    }
}

impl CodingAgent {
    /// Return the CLI flags to enable full-auto mode (no confirmation prompts).
    /// Each agent has its own mechanism:
    /// - Claude Code: `--dangerously-skip-permissions`
    /// - Codex: `--full-auto`
    /// - Aider: `--yes-always`
    /// - OpenCode: (no known flag)
    pub fn full_auto_flags(&self) -> Vec<String> {
        match self.command.as_str() {
            "claude" => vec!["--dangerously-skip-permissions".to_string()],
            "codex" => vec!["--full-auto".to_string()],
            "aider" => vec!["--yes-always".to_string()],
            _ => vec![],
        }
    }
}

impl fmt::Display for CodingAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name)
    }
}

/// Resume strategy for a coding agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeStrategy {
    /// Pass `--resume` flag (e.g., `claude --resume`)
    ResumeFlag,
    /// Resume via session subcommand (e.g., `goose session resume <id>`)
    SessionResume,
    /// No built-in resume support
    None,
}

/// Known coding agents and their CLI commands.
struct AgentDef {
    name: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    resume: ResumeStrategy,
}

/// Only agents that support system prompt injection are included.
/// Ryve requires control over the Hand's instructions — agents without
/// a system prompt flag cannot be reliably coordinated.
const KNOWN_AGENTS: &[AgentDef] = &[
    AgentDef { name: "Claude Code", command: "claude", args: &[], resume: ResumeStrategy::ResumeFlag },
    AgentDef { name: "Codex", command: "codex", args: &[], resume: ResumeStrategy::ResumeFlag },
    AgentDef { name: "Aider", command: "aider", args: &[], resume: ResumeStrategy::None },
    AgentDef { name: "OpenCode", command: "opencode", args: &[], resume: ResumeStrategy::None },
];

/// Detect which coding agents are available on PATH.
pub fn detect_available() -> Vec<CodingAgent> {
    KNOWN_AGENTS
        .iter()
        .filter(|def| which(def.command))
        .map(|def| CodingAgent {
            display_name: def.name.to_string(),
            command: def.command.to_string(),
            args: def.args.iter().map(|a| a.to_string()).collect(),
            resume: def.resume.clone(),
        })
        .collect()
}

/// Check if a command exists on PATH.
fn which(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
