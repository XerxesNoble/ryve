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
    /// Compatibility status with Ryve's known-good version range. Populated
    /// at detection time by [`detect_available`]; inline constructors used
    /// by tests / fallback paths default to [`CompatStatus::Unknown`].
    ///
    /// See spark `ryve-133ebb9b`: CLI flag surfaces drift between releases
    /// (e.g. codex removed `--instructions`, opencode renamed its run
    /// subcommand). Ryve runs `<cmd> --version` on detection so the UI can
    /// nudge the user to upgrade before a Hand spawn fails cryptically.
    pub compatibility: CompatStatus,
}

/// Result of comparing the installed CLI version against Ryve's known-good
/// range. The variant carries enough context for the UI to render a clear
/// upgrade prompt without consulting any other state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CompatStatus {
    /// Detected version is inside the supported range.
    Compatible {
        /// Parsed `MAJOR.MINOR.PATCH` string for display.
        version: String,
    },
    /// Detected version is outside the supported range. The user should
    /// upgrade (or, more rarely, downgrade) before relying on this agent.
    Unsupported {
        /// Parsed `MAJOR.MINOR.PATCH` string for display.
        version: String,
        /// Human-readable explanation suitable for a toast or modal: names
        /// the agent, the detected version, and the required range.
        reason: String,
    },
    /// Version probe failed (binary missing `--version`, garbled output,
    /// or no known range registered for this command). Treated as a soft
    /// warning: the UI may still let the user pick the agent but should
    /// surface that compatibility could not be confirmed.
    #[default]
    Unknown,
}

impl CompatStatus {
    /// True iff the agent is known to be unsupported. Use this to gate
    /// "Spawn" buttons in pickers.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, CompatStatus::Unsupported { .. })
    }

}

impl CodingAgent {
    /// Build the command + args to resume a previous session.
    /// Returns None if the agent doesn't support resumption.
    pub fn resume_args(&self, resume_id: Option<&str>) -> Option<(String, Vec<String>)> {
        match &self.resume {
            ResumeStrategy::ResumeFlag => {
                // `claude --resume` or `codex --resume`
                //
                // We deliberately do NOT pass the previous session id as a
                // positional argument here. With just `--resume`, claude/codex
                // launch their own interactive session picker so the user
                // can confirm which conversation to resume — that's what we
                // want from the Hand panel's "▶ resume" button. Passing the
                // id would auto-resume silently and bypass the picker.
                let _ = resume_id;
                let mut args = self.args.clone();
                args.push("--resume".to_string());
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
                // message is the trailing positional.
                //
                // Historical bug (sp-ryve-3eed113f): we used to also pass
                // `--system-prompt <prompt_file>`, but `--system-prompt` takes
                // an inline string, not a file path. Claude received the
                // literal path string as its system role, which combined with
                // the "Read these rules carefully" framing in HOUSE_RULES
                // made it intermittently reply with a one-word
                // acknowledgement ("Understood." / "Acknowledged.") and
                // exit immediately. The full briefing now goes through as
                // the user message; agent_prompts.rs leads with an explicit
                // EXECUTE-NOW header so claude does not treat it as a
                // reading-comprehension exercise.
                args.push("--print".to_string());
                args.push("--dangerously-skip-permissions".to_string());
                args.push(prompt.to_string());
            }
            "codex" => {
                // `codex exec <prompt>` runs once non-interactively.
                //
                // We MUST NOT use `--full-auto` here. `--full-auto` is a
                // shortcut for `--sandbox workspace-write`, which only allows
                // writes inside the agent's cwd. Hands run with cwd set to
                // their per-Hand worktree at `.ryve/worktrees/<id>/`, but the
                // workshop sqlite DB lives one level up at `.ryve/sparks.db`.
                // workspace-write therefore makes the DB readonly from the
                // Hand's perspective, and every `ryve spark status` /
                // `ryve spark close` / `ryve comment add` /
                // `ryve assign heartbeat` call fails with
                // "attempt to write a readonly database". The Hand looks
                // stuck/dead from the orchestrator side and the Head has to
                // release+respawn it (see spark ryve-f232cc66 / repro hand
                // 19c282fd-2402-45e2-a8d2-6aaf7df7f6e5 on ryve-1483878d).
                //
                // Ryve already places each Hand in an isolated git worktree
                // and trusts its own coordinated agents — the same trust
                // model that lets us pass `--dangerously-skip-permissions`
                // to claude. Use codex's equivalent so the Hand can write
                // to the workshop DB outside its cwd.
                args.push("exec".to_string());
                args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
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
    /// - Codex: `--dangerously-bypass-approvals-and-sandbox` (see note below)
    /// - Aider: `--yes-always`
    /// - OpenCode: (no known flag)
    ///
    /// NOTE on codex: we deliberately do NOT use `--full-auto` here. That
    /// flag implies `--sandbox workspace-write`, which makes anything
    /// outside the Hand's cwd readonly. The workshop sqlite DB lives at
    /// `.ryve/sparks.db` (above the Hand's worktree at
    /// `.ryve/worktrees/<id>/`), so workspace-write blocks every ryve CLI
    /// write the Hand needs to make. See spark ryve-f232cc66.
    pub fn full_auto_flags(&self) -> Vec<String> {
        match self.command.as_str() {
            "claude" => vec!["--dangerously-skip-permissions".to_string()],
            "codex" => vec!["--dangerously-bypass-approvals-and-sandbox".to_string()],
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

/// Inclusive lower bound / exclusive upper bound semver range used to
/// validate the installed CLI version against Ryve's known-good window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionRange {
    /// Inclusive minimum `(major, minor, patch)`.
    pub min: (u32, u32, u32),
    /// Optional exclusive upper bound. `None` means "no upper limit".
    pub max_exclusive: Option<(u32, u32, u32)>,
}

impl VersionRange {
    fn contains(&self, v: (u32, u32, u32)) -> bool {
        if v < self.min {
            return false;
        }
        if let Some(max) = self.max_exclusive {
            if v >= max {
                return false;
            }
        }
        true
    }

    fn describe(&self) -> String {
        let (a, b, c) = self.min;
        let mut s = format!(">= {a}.{b}.{c}");
        if let Some((x, y, z)) = self.max_exclusive {
            s.push_str(&format!(", < {x}.{y}.{z}"));
        }
        s
    }
}

/// Look up Ryve's known-good range for a given CLI command. Returns `None`
/// for commands Ryve has no opinion about (custom user agents, test stubs).
pub fn known_range(command: &str) -> Option<VersionRange> {
    // The minima encode the contract documented inline in this file:
    //   * `codex >= 0.118` — `--instructions` was removed; Ryve now relies
    //     on `codex exec` + AGENTS.md instead.
    //   * `opencode >= 1.2` — same AGENTS.md flow.
    //   * `claude >= 1.0` — `--print` + `--system-prompt` headless mode.
    //   * `aider >= 0.50` — `--message` / `--read` flag combo.
    // Bumping these is the canonical way to tell users "your CLI is too old".
    match command {
        "codex" => Some(VersionRange { min: (0, 118, 0), max_exclusive: None }),
        "opencode" => Some(VersionRange { min: (1, 2, 0), max_exclusive: None }),
        "claude" => Some(VersionRange { min: (1, 0, 0), max_exclusive: None }),
        "aider" => Some(VersionRange { min: (0, 50, 0), max_exclusive: None }),
        _ => None,
    }
}

/// Find the first `MAJOR.MINOR.PATCH` triple inside `s`. Tolerant of the
/// surrounding text variations across `--version` outputs (e.g. `codex
/// 0.118.3`, `aider 0.71.0`, `1.2.3 (opencode)`, `Claude Code 1.0.50`).
pub fn parse_first_semver(s: &str) -> Option<(u32, u32, u32)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        let candidate = &s[start..i];
        let parts: Vec<&str> = candidate.split('.').collect();
        if parts.len() >= 3 {
            if let (Ok(a), Ok(b), Ok(c)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                // Strip any trailing non-digit (e.g. "0-rc1") for the patch.
                parts[2]
                    .trim_end_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u32>(),
            ) {
                return Some((a, b, c));
            }
        }
        // Skip past this candidate to avoid an infinite loop on malformed
        // input that started with a digit but couldn't be parsed.
        if i == start {
            i += 1;
        }
    }
    None
}

/// Run `<command> --version` and return its combined output. Returns `None`
/// if the binary failed to launch or exited non-zero. Some CLIs print to
/// stderr instead of stdout (notably `aider`), so both streams are merged.
fn run_version_command(command: &str) -> Option<String> {
    let out = std::process::Command::new(command)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    if combined.trim().is_empty() {
        combined = String::from_utf8_lossy(&out.stderr).into_owned();
    }
    Some(combined)
}

/// Pure version-comparison helper used by [`check_compatibility`] and tests.
/// Separated so unit tests can exercise the matrix without forking processes.
pub fn evaluate_version(
    command: &str,
    version_output: &str,
) -> CompatStatus {
    let Some(range) = known_range(command) else {
        return CompatStatus::Unknown;
    };
    let Some(v) = parse_first_semver(version_output) else {
        return CompatStatus::Unknown;
    };
    let detected = format!("{}.{}.{}", v.0, v.1, v.2);
    if range.contains(v) {
        CompatStatus::Compatible { version: detected }
    } else {
        let reason = format!(
            "Ryve requires `{command}` {req}, but detected v{detected}. \
             Upgrade the CLI — flag-surface changes between releases break \
             Hand spawning.",
            req = range.describe(),
        );
        CompatStatus::Unsupported {
            version: detected,
            reason,
        }
    }
}

/// Probe the installed CLI's version and classify it against Ryve's known
/// range. Used by [`detect_available`] at boot.
pub fn check_compatibility(command: &str) -> CompatStatus {
    if known_range(command).is_none() {
        return CompatStatus::Unknown;
    }
    let Some(out) = run_version_command(command) else {
        return CompatStatus::Unknown;
    };
    evaluate_version(command, &out)
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

/// Detect which coding agents are available on PATH. Each detected agent
/// has its `compatibility` field populated by probing `<cmd> --version`.
pub fn detect_available() -> Vec<CodingAgent> {
    KNOWN_AGENTS
        .iter()
        .filter(|def| which(def.command))
        .map(|def| {
            let mut agent = def_to_agent(def);
            agent.compatibility = check_compatibility(&agent.command);
            agent
        })
        .collect()
}

/// Return every coding agent Ryve knows about, regardless of whether it is
/// currently installed. Used by `ryve hand spawn --agent <name>` so the
/// caller can resolve a name to a definition without first running `which`.
///
/// `compatibility` is left as [`CompatStatus::Unknown`] — callers that care
/// (e.g. the GUI's boot probe) should run [`check_compatibility`] explicitly
/// rather than paying for a `--version` fork on every CLI invocation.
pub fn known_agents() -> Vec<CodingAgent> {
    KNOWN_AGENTS.iter().map(def_to_agent).collect()
}

fn def_to_agent(def: &AgentDef) -> CodingAgent {
    CodingAgent {
        display_name: def.name.to_string(),
        command: def.command.to_string(),
        args: def.args.iter().map(|a| a.to_string()).collect(),
        resume: def.resume.clone(),
        compatibility: CompatStatus::Unknown,
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
    fn build_headless_args_claude_uses_print_mode_without_system_prompt_path() {
        // Regression for sp-ryve-3eed113f: claude must not be passed
        // `--system-prompt <prompt_file>` because that flag takes an inline
        // string, not a file path. Doing so put the literal path string into
        // claude's system role and made it intermittently exit with a
        // one-word acknowledgement instead of starting the assignment.
        let agent = agent_for("claude");
        let path = PathBuf::from("/tmp/p.md");
        let args = agent.build_headless_args("hello", &path);
        assert!(
            args.iter().any(|a| a == "--print"),
            "claude needs --print: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--dangerously-skip-permissions"),
            "claude needs --dangerously-skip-permissions: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--system-prompt"),
            "claude must NOT receive --system-prompt: that flag takes an \
             inline string, not the prompt file path: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "/tmp/p.md"),
            "claude args must not contain the prompt file path literal: {args:?}"
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
        // Regression for spark ryve-f232cc66: codex Hands MUST run with the
        // sandbox bypassed, otherwise the workshop sqlite DB (which lives
        // outside the Hand's worktree cwd) is readonly and every ryve CLI
        // write fails. `--full-auto` would re-introduce that bug because it
        // implies `--sandbox workspace-write`.
        assert!(
            args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()),
            "codex headless args must bypass the sandbox so the Hand can \
             write to the workshop DB outside its worktree: {args:?}"
        );
        assert!(
            !args.contains(&"--full-auto".to_string()),
            "codex headless args must NOT use --full-auto (workspace-write \
             sandbox makes the workshop DB readonly): {args:?}"
        );
        assert_eq!(args.last().map(String::as_str), Some("do the thing"));
    }

    /// Same regression check via `full_auto_flags`, the public helper that
    /// other call sites (e.g. interactive spawn paths in `workshop`) use to
    /// build a codex command line. If this ever drifts back to `--full-auto`
    /// the readonly-DB bug returns.
    #[test]
    fn full_auto_flags_codex_bypasses_sandbox() {
        let agent = agent_for("codex");
        let flags = agent.full_auto_flags();
        assert_eq!(flags, vec!["--dangerously-bypass-approvals-and-sandbox".to_string()]);
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
            compatibility: CompatStatus::Unknown,
        };
        let args = agent.build_headless_args("stub-prompt", &PathBuf::from("/tmp/x"));
        assert_eq!(
            args,
            vec!["--baseline".to_string(), "stub-prompt".to_string()]
        );
    }

    // ── Version-detection tests (spark ryve-133ebb9b) ───────────────────

    #[test]
    fn parse_first_semver_extracts_triple_from_varied_outputs() {
        // Real-world `--version` output samples for each supported CLI.
        let cases = [
            ("codex 0.118.3", (0, 118, 3)),
            ("1.2.4 (opencode)", (1, 2, 4)),
            ("aider 0.71.0", (0, 71, 0)),
            ("Claude Code 1.0.50 (build abc)", (1, 0, 50)),
            ("v2.0.0-rc1", (2, 0, 0)),
        ];
        for (input, expected) in cases {
            assert_eq!(
                parse_first_semver(input),
                Some(expected),
                "failed to parse {input:?}",
            );
        }
    }

    #[test]
    fn parse_first_semver_returns_none_for_garbage() {
        assert_eq!(parse_first_semver(""), None);
        assert_eq!(parse_first_semver("no version here"), None);
        // Two-component "version" should not match.
        assert_eq!(parse_first_semver("codex 0.118"), None);
    }

    #[test]
    fn version_range_contains_inclusive_min_and_exclusive_max() {
        let r = VersionRange {
            min: (1, 0, 0),
            max_exclusive: Some((2, 0, 0)),
        };
        assert!(r.contains((1, 0, 0)), "min is inclusive");
        assert!(r.contains((1, 99, 99)));
        assert!(!r.contains((0, 99, 99)));
        assert!(!r.contains((2, 0, 0)), "max is exclusive");
    }

    #[test]
    fn evaluate_version_marks_old_codex_as_unsupported() {
        // Codex 0.117 lacks the AGENTS.md flow Ryve depends on; the user
        // must upgrade. The reason string should mention both the detected
        // version and the requirement so the toast is actionable.
        let status = evaluate_version("codex", "codex 0.117.0");
        match status {
            CompatStatus::Unsupported { version, reason } => {
                assert_eq!(version, "0.117.0");
                assert!(reason.contains("0.117.0"), "reason missing detected version: {reason}");
                assert!(reason.contains("0.118"), "reason missing required min: {reason}");
                assert!(reason.contains("codex"), "reason missing agent name: {reason}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_version_accepts_current_codex() {
        let status = evaluate_version("codex", "codex 0.118.3");
        assert!(matches!(
            status,
            CompatStatus::Compatible { ref version } if version == "0.118.3"
        ));
    }

    #[test]
    fn evaluate_version_accepts_future_codex() {
        // No upper bound — future releases stay compatible until Ryve says
        // otherwise. Regression guard against accidentally adding a max.
        let status = evaluate_version("codex", "codex 99.0.0");
        assert!(matches!(status, CompatStatus::Compatible { .. }));
    }

    #[test]
    fn evaluate_version_handles_unknown_command() {
        // Custom user agents have no registered range — must fall through
        // to Unknown rather than panicking or claiming Compatible.
        assert_eq!(
            evaluate_version("my-custom-tool", "my-custom-tool 1.2.3"),
            CompatStatus::Unknown,
        );
    }

    #[test]
    fn evaluate_version_handles_unparseable_output() {
        assert_eq!(
            evaluate_version("codex", "version: yes"),
            CompatStatus::Unknown,
        );
    }

    #[test]
    fn compat_status_is_unsupported_only_for_unsupported_variant() {
        assert!(!CompatStatus::Compatible { version: "1.0.0".into() }.is_unsupported());
        assert!(!CompatStatus::Unknown.is_unsupported());
        assert!(CompatStatus::Unsupported {
            version: "0.1.0".into(),
            reason: "old".into(),
        }
        .is_unsupported());
    }

    #[test]
    fn known_range_covers_all_first_party_agents() {
        // Regression: every agent in KNOWN_AGENTS should have a registered
        // version range, otherwise detect_available will silently mark it
        // Unknown forever.
        for agent in known_agents() {
            assert!(
                known_range(&agent.command).is_some(),
                "no version range registered for first-party agent {}",
                agent.command,
            );
        }
    }
}
