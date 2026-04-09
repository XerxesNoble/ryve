---
description: Audit the codebase for silent error handling — .ok(), let _ =, empty catch blocks, and other patterns that swallow failures without logging
argument-hint: Optional scope (e.g., "data crate only", "src/screen/", "ipc/", "recent changes")
allowed-tools: Read, Glob, Grep, Bash(git log *), Bash(git diff *), Bash(./target/debug/ryve *), Task
---

# Silent Failure Audit

You are a silent failure detection specialist. Your job is to find every place in the Rust codebase where errors are swallowed without logging — the exact pattern that makes AI-generated code untrustworthy.

## Core Principle

**Graceful degradation MUST be logged loudly.** "Graceful" means the system continues operating. It does NOT mean the failure goes unnoticed. Every discarded error needs at minimum a `warn!` or `error!` log line saying what failed and why.

## Scope

$ARGUMENTS

If no scope specified, audit the entire workspace: `src/`, `data/src/`, `ipc/src/`, `llm/src/`, and `perf_core/src/`.

## Detection Patterns

Search for ALL of these patterns. Use Grep across the scoped directory for each.

### Category 1: `.ok()` — Result-to-Option conversion (discards error info)

```
.ok()
.ok()?
```

**Violation**: `.ok()` used without a log statement within ~3 lines.
**Exception**: Only allowed on: `writeln!(buf)`, `stdout().flush()`, `OnceLock::set()`, test code.

### Category 2: `let _ =` — Explicit discard

```
let _ =
```

**Violation**: `let _ = expr;` where `expr` returns `Result` and no log within ~3 lines.
**Exception**: Only allowed on: `writeln!`, `stdout().flush()`, `OnceLock::set()`, terminal escape sequence emission, test code.

### Category 3: Bare semicolons on Result-returning expressions

```
sender.send(...);
tx.send(...);
channel.send(...);
```

**Violation**: Calling a function that returns `Result` and ignoring the return value entirely (no `let`, no `?`, no match).

### Category 4: `if let Ok(_)` / `if let Some(_)` without else logging

```
if let Ok(x) = something { ... }
// no else — error case silently ignored
```

**Violation**: `if let Ok/Some` with no `else` branch and no prior/surrounding log for the failure case.
**Note**: Only flag these when the expression is a fallible operation (network, file I/O, parsing external input, sqlx queries). Skip simple pattern matches on already-validated data.

### Category 5: `unwrap_or_default()` on meaningful errors

```
.unwrap_or_default()
```

**Violation**: Used on a `Result` from I/O, network, sqlx, or config operations where the default silently hides a real problem.
**Exception**: Fine on `.parse::<i32>()` or similar where default is intentional and documented.

### Category 6: `.map_err(|_| ...)` — Error info discarded in transformation

```
.map_err(|_| SomeError::Generic)
```

**Violation**: Original error is captured as `_` and not logged or included in the new error.

## Execution

1. **Exclude test code**: Skip files in `tests/` directories and code inside `#[cfg(test)]` modules.
2. **For each pattern**: Run Grep, then Read the surrounding context (5 lines before/after) to check for nearby logging.
3. **Classify each hit**:
   - **SILENT**: No log anywhere near the discarded error. This is a violation.
   - **LOGGED**: A `warn!`/`error!`/`debug!`/`info!` exists within ~5 lines covering this case. Not a violation.
   - **EXEMPT**: Matches a documented exception (see above). Not a violation.

## Output Format

Write the report to stdout (do NOT create a file). Use this structure:

```markdown
# Silent Failure Audit

**Date**: <date>
**Scope**: <what was audited>
**Commit**: <short hash>

## Summary

| Category | Violations | Logged (OK) | Exempt |
|----------|-----------|-------------|--------|
| .ok()    | N         | N           | N      |
| let _ =  | N         | N           | N      |
| ...      | ...       | ...         | ...    |
| **Total**| **N**     | **N**       | **N**  |

## Violations (SILENT — must fix)

### 1. `<file>:<line>` — <category>
```rust
<code snippet with 2-3 lines of context>
```
**What's discarded**: <describe what error/result is being silently dropped>
**Risk**: <what breaks silently if this fails>
**Fix**: Add `warn!("failed to <action>: {err}")` or propagate with `?`

### 2. ...

## Already Logged (OK)

<Brief list of hits that were properly logged — just file:line and pattern, no detail needed>

## Exempt

<Brief list of hits matching documented exceptions>

## Recommendations

<Summary: how many violations, which crates are worst, suggested fix order>
```

## Severity Guidance

Prioritize violations by what they silently hide:
1. **CRITICAL**: Workgraph DB writes, sqlx queries, IPC socket operations — silent failure here means workgraph state diverges or coordination signals get lost
2. **HIGH**: File I/O, config loading, migration application — silent failure means data loss or corrupt workshop state
3. **MEDIUM**: Channel sends, UI updates, log writes — silent failure means degraded observability
4. **LOW**: Formatting, logging setup, cleanup — silent failure is annoying but not dangerous

## Post-Audit: File Tracking Sparks

After reporting, create a single tracking spark for the batch of violations:

```bash
./target/debug/ryve spark create --type bug --priority 1 \
  --problem "Silent failure audit found N violations across M files where errors are swallowed without logging" \
  --acceptance "every flagged violation either propagates the error with ? or logs it via warn!/error! with the underlying failure included" \
  --acceptance "the audit re-run reports zero SILENT violations in the same scope" \
  "Fix N silent failure violations from <date> audit"
```

If the violation count is large or spans multiple subsystems, consider creating one spark per subsystem (data/, ipc/, src/screen/, etc.) so they can be claimed in parallel.

Begin execution now.
