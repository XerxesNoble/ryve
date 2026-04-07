// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Auto-detection of CLI coding agents available on the system.

use std::fmt;
use std::path::Path;

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
                Some((
                    self.command.clone(),
                    vec!["session".into(), "resume".into(), id.to_string()],
                ))
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
            "aider" => Some(("--read", true)),
            // codex (>=0.118) and opencode (>=1.2) have no system-prompt CLI
            // flag — they read AGENTS.md from cwd instead. Ryve drops an
            // AGENTS.md into the hand worktree (see create_hand_worktree).
            _ => None,
        }
    }
}

impl CodingAgent {
    /// Build the complete arg list for invoking this agent in **headless
    /// (one-shot) mode** with the given user prompt.
    ///
    /// This is the canonical entry point used by `hand_spawn::spawn_hand`
    /// to launch a detached Hand. It handles three things at once that the
    /// caller previously had to wire up by hand:
    ///
    ///   1. Whatever subcommand or flag puts the agent into non-interactive
    ///      print mode (`claude --print`, `codex exec`, `aider --message`,
    ///      `opencode run`).
    ///   2. Full-auto / skip-permission flags so the agent does not stall
    ///      waiting for confirmation.
    ///   3. The actual **user prompt** — passed positionally or via the
    ///      agent-specific flag — so the agent has something to act on.
    ///      This is the bit the original implementation was missing: it
    ///      only injected `--system-prompt`, which sets the system role and
    ///      does **not** satisfy the user-message requirement of print mode.
    ///      Claude logged `Input must be provided either through stdin or
    ///      as a prompt argument when using --print` and exited immediately,
    ///      taking every Hand with it.
    ///
    /// `prompt` is the inline text. `prompt_path` is a file already
    /// containing that same text — used as the system-prompt file for
    /// agents (claude, aider) that want a path rather than inline.
    ///
    /// Begins with `self.args` so any baseline flags from the agent
    /// definition are preserved. Unknown commands fall through to passing
    /// the prompt as a single trailing positional argument so that custom
    /// agents and test stubs (e.g. a tiny `bash` script) still receive it.
    pub fn build_headless_args(&self, prompt: &str, prompt_path: &Path) -> Vec<String> {
        let mut args = self.args.clone();
        let prompt_file = prompt_path.to_string_lossy().into_owned();

        match self.command.as_str() {
            "claude" => {
                // `claude --print` is the documented headless mode. The user
                // message is the trailing positional. `--system-prompt` keeps
                // the spark briefing as the system role.
                args.push("--print".to_string());
                args.push("--dangerously-skip-permissions".to_string());
                args.push("--system-prompt".to_string());
                args.push(prompt_file);
                args.push(prompt.to_string());
            }
            "codex" => {
                // `codex exec <prompt>` runs once non-interactively.
                args.push("exec".to_string());
                args.push("--full-auto".to_string());
                args.push(prompt.to_string());
            }
            "aider" => {
                // `aider --message` sends a single user message and exits.
                // `--read` makes the prompt file part of the read-only
                // context so the system briefing is also visible.
                args.push("--yes-always".to_string());
                args.push("--read".to_string());
                args.push(prompt_file);
                args.push("--message".to_string());
                args.push(prompt.to_string());
            }
            "opencode" => {
                // `opencode run <prompt>` is opencode's headless command.
                args.push("run".to_string());
                args.push(prompt.to_string());
            }
            _ => {
                // Unknown / custom / test-stub command. Pass the prompt as a
                // trailing positional so the binary at least sees it. Tests
                // rely on this to assert the prompt was delivered.
                args.push(prompt.to_string());
            }
        }

        args
    }

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
    AgentDef {
        name: "Claude Code",
        command: "claude",
        args: &[],
        resume: ResumeStrategy::ResumeFlag,
    },
    AgentDef {
        name: "Codex",
        command: "codex",
        args: &[],
        resume: ResumeStrategy::ResumeFlag,
    },
    AgentDef {
        name: "Aider",
        command: "aider",
        args: &[],
        resume: ResumeStrategy::None,
    },
    AgentDef {
        name: "OpenCode",
        command: "opencode",
        args: &[],
        resume: ResumeStrategy::None,
    },
];

/// Detect which coding agents are available on PATH.
pub fn detect_available() -> Vec<CodingAgent> {
    KNOWN_AGENTS
        .iter()
        .filter(|def| which(def.command))
        .map(def_to_agent)
        .collect()
}

/// Return every coding agent Ryve knows about, regardless of whether it is
/// currently installed. Used by `ryve hand spawn --agent <name>` so the
/// caller can resolve a name to a definition without first running `which`.
pub fn known_agents() -> Vec<CodingAgent> {
    KNOWN_AGENTS.iter().map(def_to_agent).collect()
}

fn def_to_agent(def: &AgentDef) -> CodingAgent {
    CodingAgent {
        display_name: def.name.to_string(),
        command: def.command.to_string(),
        args: def.args.iter().map(|a| a.to_string()).collect(),
        resume: def.resume.clone(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Helper: build a stock agent definition by command name.
    fn agent_for(cmd: &str) -> CodingAgent {
        known_agents()
            .into_iter()
            .find(|a| a.command == cmd)
            .unwrap_or_else(|| panic!("known agent {cmd} missing"))
    }

    /// Regression for spark ryve-b3ad7bd1: every supported agent must
    /// receive the user prompt as part of its argv when launched in
    /// headless mode. The previous implementation only set
    /// `--system-prompt`, which left claude (and the others) with no user
    /// message and caused them to exit immediately.
    #[test]
    fn build_headless_args_includes_prompt_for_every_supported_agent() {
        let prompt = "ASSIGNMENT: spark sp-test-1234\nbegin work";
        let path = PathBuf::from("/tmp/ryve-test-prompt.md");

        for cmd in ["claude", "codex", "aider", "opencode"] {
            let agent = agent_for(cmd);
            let args = agent.build_headless_args(prompt, &path);
            assert!(
                args.iter().any(|a| a == prompt),
                "{cmd} headless args missing user prompt: {args:?}"
            );
        }
    }

    #[test]
    fn build_headless_args_claude_uses_print_mode_with_system_prompt_file() {
        let agent = agent_for("claude");
        let path = PathBuf::from("/tmp/p.md");
        let args = agent.build_headless_args("hello", &path);
        assert!(args.iter().any(|a| a == "--print"), "claude needs --print: {args:?}");
        assert!(
            args.iter().any(|a| a == "--system-prompt"),
            "claude should still pass --system-prompt: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "/tmp/p.md"),
            "claude should reference the prompt file: {args:?}"
        );
        assert_eq!(
            args.last().map(String::as_str),
            Some("hello"),
            "user prompt must be the trailing positional for claude"
        );
    }

    #[test]
    fn build_headless_args_codex_uses_exec_subcommand() {
        let agent = agent_for("codex");
        let args = agent.build_headless_args("do the thing", &PathBuf::from("/tmp/x"));
        assert_eq!(args.first().map(String::as_str), Some("exec"));
        assert!(args.contains(&"--full-auto".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("do the thing"));
    }

    #[test]
    fn build_headless_args_aider_uses_message_flag() {
        let agent = agent_for("aider");
        let args = agent.build_headless_args("fix it", &PathBuf::from("/tmp/x"));
        assert!(args.contains(&"--message".to_string()));
        assert!(args.contains(&"--yes-always".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("fix it"));
    }

    #[test]
    fn build_headless_args_opencode_uses_run_subcommand() {
        let agent = agent_for("opencode");
        let args = agent.build_headless_args("ship it", &PathBuf::from("/tmp/x"));
        assert_eq!(args.first().map(String::as_str), Some("run"));
        assert_eq!(args.last().map(String::as_str), Some("ship it"));
    }

    /// Custom / unknown agents (test stubs, user-defined wrappers) must
    /// still receive the prompt — they fall through to a trailing
    /// positional. Tests downstream rely on this so a stub script can
    /// observe what the agent would have received.
    #[test]
    fn build_headless_args_unknown_command_appends_prompt_positionally() {
        let agent = CodingAgent {
            display_name: "stub".into(),
            command: "/usr/local/bin/stub-agent".into(),
            args: vec!["--baseline".into()],
            resume: ResumeStrategy::None,
        };
        let args = agent.build_headless_args("stub-prompt", &PathBuf::from("/tmp/x"));
        assert_eq!(args, vec!["--baseline".to_string(), "stub-prompt".to_string()]);
    }
}
