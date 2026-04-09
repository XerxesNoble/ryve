#!/usr/bin/env bash
# Guard: refuse to let sparks.db or its SQLite sidecars be staged/tracked.
#
# SQLite treats sparks.db, sparks.db-wal, and sparks.db-shm as a single atomic
# unit. Versioning any one of them (or a stale subset) guarantees torn state
# on checkout/stash and corrupts the Ryve workgraph. See docs/WORKGRAPH.md.
#
# Usage:
#   scripts/check-sparks-db-not-tracked.sh           # checks HEAD + index
#   scripts/check-sparks-db-not-tracked.sh --staged  # checks only the index
#                                                    # (used by pre-commit)
set -euo pipefail

mode="${1:-all}"

# Pattern matches sparks.db and every SQLite sidecar variant (-wal, -shm,
# -journal, -journalNNN, plus any future extensions).
pattern='(^|/)sparks\.db([-.][A-Za-z0-9_-]+)?$'

fail=0

check_list() {
    local label="$1"
    local list="$2"
    if [[ -z "$list" ]]; then
        return 0
    fi
    local matches
    matches="$(printf '%s\n' "$list" | grep -E "$pattern" || true)"
    if [[ -n "$matches" ]]; then
        echo "error: $label contains sparks.db file(s) that must never be versioned:" >&2
        printf '  %s\n' $matches >&2
        fail=1
    fi
}

# Files staged for commit.
staged="$(git diff --cached --name-only --diff-filter=AM 2>/dev/null || true)"
check_list "staged index" "$staged"

if [[ "$mode" != "--staged" ]]; then
    # Files currently tracked in HEAD (catches anything that slipped in
    # historically — e.g. the 2026-04-08 incident).
    tracked="$(git ls-files 2>/dev/null || true)"
    check_list "git tracked files" "$tracked"
fi

if (( fail )); then
    cat >&2 <<'EOF'

These files are SQLite state owned by a live process. Tracking them causes
atomic-unit skew and corrupts the workgraph on stash/checkout.

Fix:
  git rm --cached <file>    # untrack without deleting from disk
  # confirm .gitignore covers .ryve/sparks.db and .ryve/sparks.db-*

See docs/WORKGRAPH.md -> "Sidecars must never be tracked".
EOF
    exit 1
fi
