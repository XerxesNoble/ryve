Stage all changes, commit with an AI-generated message via xmit, fix any lefthook errors, and push to the remote.

## Steps

1. **Check branch**: Run `git branch --show-current`. If on `main`, stop and ask the user to switch branches.

2. **Check for changes**: Run `git status --short`. If there are no changes, say so and stop.

3. **Stage everything**: Run `git add -A`.

4. **Commit via xmit**: Run `xmit -y` to auto-commit with an AI-generated conventional commit message.

5. **Fix lefthook failures**: If xmit/commit fails due to pre-commit hooks (fmt, clippy, eslint, prettier, tsc, ruff):
   - Read the error output carefully
   - Fix the root cause (don't just suppress the check)
   - Re-stage fixed files with `git add -A`
   - Run `xmit -y` again (this creates a NEW commit since the previous one didn't happen)
   - Repeat until the commit succeeds

6. **Push**: Run `git push origin $(git branch --show-current)` to push the current branch.
   - If push fails due to pre-push hooks (tests, cargo vet), fix the failures and retry
   - Never force-push. If the remote has diverged, ask the user how to proceed.

7. **Confirm**: Show the final `git log --oneline -3` and `git status` to verify everything is clean.

## Important

- Never force-push or use `--no-verify`
- Never commit to `main` — always check the branch first
- If lefthook auto-fixes files (fmt, prettier, ruff), those fixes are already staged — just re-run `xmit -y`
- Fix lint/format issues in the code itself, not by disabling checks
- The `xmit` tool handles staging and commit message generation — don't draft your own message
