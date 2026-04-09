---
description: Run a full security and correctness review of the codebase and write the report to docs/security-reviews/
argument-hint: Optional focus area (e.g., "ipc only", "the new IPC subscription", "data crate changes since last review")
allowed-tools: Read, Glob, Grep, Bash(git log *), Bash(git rev-parse *), Bash(git diff *), Bash(cargo clippy *), Bash(cargo test *), Bash(./target/debug/ryve *), Task
---

# Security Review

You are a security review orchestrator for Ryve. Your job is to conduct a comprehensive security and correctness review, then produce a detailed report at `docs/security-reviews/<date>.md`.

## Context

$ARGUMENTS

## Review Methodology

Use **four parallel code-reviewer agents** (via the Task tool with `subagent_type: "code-reviewer"`) to review different subsystems simultaneously. Each agent should focus on bugs, logic errors, security vulnerabilities, and correctness issues with **confidence >= 80%**.

### Prior Reviews

Always read the most recent review in `docs/security-reviews/` first to:
1. Understand the existing review format — match it exactly
2. Track which prior findings are still open vs. resolved
3. Avoid re-reporting already-documented issues (unless status changed)

### Agent Assignments

Launch these four agents **in parallel**:

#### Agent A: Agent Execution & Spawn Layer
Scope: `src/hand_spawn.rs`, `src/agent_prompts.rs`, `src/coding_agents.rs`, `src/delegation.rs`, `src/worktree_cleanup.rs`

Prompt: "Deep security and correctness review of Ryve's agent execution and spawn subsystem. Focus on: subprocess argument construction (shell injection in spawned coding-agent commands), worktree isolation (cross-Hand mutation, path traversal into other workshops), prompt injection in compose_*_prompt functions (untrusted spark fields flowing into system prompts), branch name validation, git command construction safety, resource cleanup on Hand crash, and the no-orphan / role-authority invariants from the delegation contracts. Read every file in the listed scope. Report findings with file, line numbers, CWE IDs, severity (CRITICAL/HIGH/MEDIUM/LOW), confidence percentage, attack vectors, and recommended fixes. Only report findings with >= 80% confidence."

#### Agent B: Workgraph Data Layer
Scope: `data/src/sparks/`, `data/src/`, `data/migrations/`, `data/tests/`

Prompt: "Deep security and correctness review of Ryve's workgraph data layer. Focus on: sqlx query construction (any non-parameterised SQL, dynamic ORDER BY / LIMIT injection), migration safety (idempotence, irreversible drops, checksum drift on already-applied migrations — see the immutable-migrations rule), JSON metadata parsing in spark intent fields, transaction atomicity for state transitions, race conditions on the embedded SQLite db under concurrent Hand writes, FromRow column drift, and any path that bypasses the assignment/spark transition validators. Read every file in the listed scope. Report findings with file, line numbers, CWE IDs, severity, confidence percentage, attack vectors, and recommended fixes. List positive controls (parameterised queries, transaction guards) that are correctly implemented. Only report findings with >= 80% confidence."

#### Agent C: IPC, IO Surfaces & Terminal
Scope: `ipc/src/`, `src/main.rs` (IPC subscription + window/event handling), `src/screen/file_viewer.rs`, `src/screen/file_explorer.rs`, `src/screen/log_tail.rs`, `src/screen/bench.rs`, the bundled `iced_term` integration

Prompt: "Deep security and correctness review of Ryve's IPC, IO surfaces, and terminal embedding. Focus on: single-instance Unix-socket security (stale socket handling, permission of the socket file, length-prefixed JSON parsing of forwarded invocations — DoS via giant or malformed payloads, untrusted local peers), file path handling in file_explorer/file_viewer (path traversal outside the workshop, symlink escape, opening device/special files, large-file DoS), log_tail file reads (race with the Hand writing the log, partial-utf8 panic, byte caps), terminal escape sequence handling (ANSI injection from spawned subprocesses affecting the embedded iced_term, scrollback overflow, paste injection). Read every file in the listed scope. Report findings with file, line numbers, CWE IDs, severity, confidence percentage, attack vectors, and recommended fixes. Only report findings with >= 80% confidence."

#### Agent D: CLI, UI Shell, Config & Build Surfaces
Scope: `src/cli/`, `src/screen/` (excluding the IO files Agent C owns), `src/widget/`, `src/workshop.rs`, `src/style.rs`, `src/icons.rs`, `Cargo.toml` files, `.github/workflows/`, any `*.sh` and `scripts/` files in the repo

Prompt: "Deep security and correctness review of Ryve's CLI, UI shell, config, and build surfaces. Focus on: CLI argument parsing and subcommand routing (spark id injection into shell-out commands, path arg validation), config file loading and persistence (TOML/JSON parsing, write atomicity, world-readable secrets), UI message dispatch correctness (any handler that touches the filesystem or spawns a subprocess based on user input), workspace state file integrity, CI workflow safety (untrusted PR contributions running with secrets, action version pinning), and any shell scripts in the repo (curl-pipe-bash risks, integrity checks, unsafe variable expansion, command injection). Use Glob to discover all *.sh files and all files under .github/workflows/. Read every file discovered. Report findings with file, line numbers, CWE IDs, severity, confidence percentage, attack vectors, and recommended fixes. Only report findings with >= 80% confidence."

## Report Synthesis

After all four agents complete, synthesize their findings into a single cohesive report at `docs/security-reviews/<today's-date>.md`. If a review already exists for today, use a `-revN` suffix (e.g., `2026-04-08-rev2.md`).

### Report Structure

Follow this exact structure (matching prior reviews):

```markdown
# Security & Correctness Review: Ryve

**Date**: <date>
**Reviewer**: AI-Assisted Security Audit (Claude Opus 4.6, multi-agent)
**Scope**: <describe scope — full codebase or focused area>
**Methodology**: Four parallel security review agents covering: (A) agent execution/spawn, (B) workgraph data layer, (C) IPC/IO/terminal, (D) CLI/UI/build surfaces. Findings deduplicated, cross-referenced, and confidence-filtered (>=80%).
**Commit**: `<short hash>` (HEAD of main)
**Prior Review**: <date and commit of previous review>

---

## Executive Summary
<2-3 paragraph summary of changes since last review, key findings, severity table>

---

## Part 1: Verification of Prior Findings
<Table tracking each finding from the most recent prior review: ID, Issue, Status (OPEN/FIXED/PARTIAL), Notes>

---

## Part 2: New Security & Correctness Findings
<Each finding with: ID (S1, S2...), Severity, CWE, File:Lines, Confidence, code snippet, attack vector, impact, remediation>

---

## Part 3: Positive Controls
<Table of correctly-implemented security and correctness mechanisms verified during this review>

---

## Part 4: Remediation Priority
<Tables grouped by: Immediate, High Priority, Medium Priority, Low Priority, Still Open from prior>

---

## Part 5: Test Coverage Assessment
<New tests added, coverage gaps identified>

---

## Part 6: Architecture Assessment
<Assessment of any new architectural features or changes>

---

## Conclusion
<Overall security rating (letter grade), path to improvement>
```

### Synthesis Rules

1. **Deduplicate**: If multiple agents report the same issue, merge into one finding with the highest confidence
2. **Cross-reference**: Note when findings from different agents are related (e.g., Agent A finds prompt injection in compose_hand_prompt, Agent B finds the unvalidated metadata field that flows into it)
3. **Verify prior findings**: Check each finding from the most recent review — mark as FIXED (with evidence), OPEN, or PARTIAL
4. **Severity calibration**: Use CRITICAL only for issues enabling remote code execution, full sandbox escape, or arbitrary command execution from untrusted input. HIGH for code execution with preconditions, data loss, workgraph state corruption, or auth bypass. MEDIUM for defense-in-depth gaps and hardening opportunities. LOW for minor robustness issues
5. **Confidence filter**: Only include findings with >= 80% confidence in the final report
6. **Security rating**: Assign a letter grade (A through F) based on the finding profile, with specific guidance on what's needed to reach the next grade

## Pre-Flight

Before launching agents:
1. Get the current commit hash: `git rev-parse --short HEAD`
2. Check for uncommitted changes: `git diff --stat` (note in report if working tree is dirty)
3. Read the most recent review in `docs/security-reviews/` to establish the baseline
4. Run `cargo clippy --workspace -- -D warnings` to catch any static analysis issues

## Post-Review: File Tracking Sparks

After writing the report, create sparks for all CONFIRMED and MITIGATED findings at P0–P2 priority:

```bash
./target/debug/ryve spark create --type bug --priority <0|1|2> \
  --problem "<severity>, <CWE>, <file:line>. See security review <date>.md" \
  --acceptance "vulnerability remediated according to the report's recommended fix" \
  --acceptance "regression test added that exercises the previously-vulnerable path" \
  "Security: <finding title>"
```

This ensures findings are tracked across sessions and don't get lost between reviews.

Begin execution now.
