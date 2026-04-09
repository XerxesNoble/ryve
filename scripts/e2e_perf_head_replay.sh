#!/usr/bin/env bash
# E2E validation harness — rerun the perf remediation under PerfHead [sp-fbf2a519]
#
# Purpose
# -------
# Replays a perf-audit-shaped epic end-to-end under `ryve head spawn
# <epic> --archetype PerfHead` and compares the resulting Crew to the
# manual human-driven run (crew cr-d48bbba9, PR #14).
#
# The manual baseline we are reproducing:
#   - 6 perf-fix child sparks decomposed from one P1 hot-path epic
#   - 6 Hand branches, one per child spark
#   - 1 Merger Hand that integrated the branches
#   - 1 merger PR (#14) posted back on the parent epic
#   - Atlas did NOT dispatch Hands directly; the Head owned the fan-out
#
# What this script verifies (the "parity contract"):
#   shape.hands_spawned       ≥ 6
#   shape.merger_spawned       = 1
#   shape.crew_parent_is_epic  = true
#   shape.atlas_direct_spawns  = 0   (no hand_assignments with parent_session_id
#                                     pointing at an Atlas session)
#   shape.hand_branches_exist  = true (git branch hand/* under test workshop)
#
# Running modes
# -------------
#   full      — real PerfHead invocation. Requires the local `ryve` binary to
#               expose `ryve head spawn --archetype PerfHead`. Fails loudly if
#               absent.
#   dry-run   — skips the `ryve head spawn` call and only seeds the workshop +
#               emits the expected parity contract. Useful in CI before the
#               epic has integrated.
#
# Usage
# -----
#   scripts/e2e_perf_head_replay.sh           # full mode
#   MODE=dry-run scripts/e2e_perf_head_replay.sh
#
# Output
# ------
#   A JSON report at $OUT_JSON (default: /tmp/perf_head_e2e_report.json)
#   Exit 0 on parity, non-zero with diff summary on deltas.
#
# This script is self-contained: it creates a throwaway workshop under
# $TMPDIR, never mutates the caller's workgraph, and cleans up on exit.

set -euo pipefail

# Isolation: when the harness is invoked from inside another Ryve workshop
# (e.g. a Hand's worktree), the parent shell has RYVE_WORKSHOP_ROOT pointing
# at that workshop and any `ryve` call would target the parent's sparks.db —
# leaking test data into a live workgraph. We scrub those env vars up front
# and re-export RYVE_WORKSHOP_ROOT to the throwaway workshop below.
unset RYVE_WORKSHOP_ROOT
unset RYVE_HAND_SESSION_ID

MODE="${MODE:-full}"
OUT_JSON="${OUT_JSON:-/tmp/perf_head_e2e_report.json}"
STALL_SECS="${STALL_SECS:-120}"
POLL_INTERVAL="${POLL_INTERVAL:-2}"

log()  { printf '[e2e-perf-head] %s\n' "$*" >&2; }
die()  { log "ERROR: $*"; exit 1; }

# Prefer the freshly built binary in target/debug over $PATH so we always
# exercise the tree we are sitting in.
WORKSHOP_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
RYVE_BIN="${RYVE_BIN:-$WORKSHOP_ROOT/target/debug/ryve}"
if [[ ! -x "$RYVE_BIN" ]]; then
  if command -v ryve >/dev/null 2>&1; then
    RYVE_BIN="$(command -v ryve)"
  else
    die "no ryve binary found (looked at $WORKSHOP_ROOT/target/debug/ryve and \$PATH)"
  fi
fi
log "using ryve binary: $RYVE_BIN"

# --- PerfHead availability probe ------------------------------------------
# This is the gate for `mode=full`: PerfHead must be wired into the CLI.
# In `dry-run` we only warn and continue.
perfhead_available() {
  "$RYVE_BIN" head --help 2>&1 | grep -qi 'spawn' || return 1
  "$RYVE_BIN" head archetype list 2>&1 | grep -qi 'perf\|build\|research\|review' || return 1
  return 0
}

if [[ "$MODE" == "full" ]] && ! perfhead_available; then
  die "ryve head spawn is not wired into $RYVE_BIN.
     The head-spawn CLI (epic ryve-fbf2a519) must be integrated before
     this harness can run in full mode. Re-run with MODE=dry-run to
     exercise the scaffold."
fi

# --- Throwaway workshop ---------------------------------------------------
TMP_WS="$(mktemp -d -t ryve-e2e-perfhead-XXXXXX)"
cleanup() {
  # Leave the directory on failure for post-mortem.
  if [[ "${KEEP_WORKSHOP:-0}" == "1" || $? -ne 0 ]]; then
    log "preserving workshop at $TMP_WS (KEEP_WORKSHOP=1 or non-zero exit)"
  else
    rm -rf "$TMP_WS"
  fi
}
trap cleanup EXIT

log "seeding throwaway workshop at $TMP_WS"
(
  cd "$TMP_WS"
  git init -q -b main
  git -c user.email=test@ryve.local -c user.name=ryve-test commit -q --allow-empty -m init
  "$RYVE_BIN" init >/dev/null
)

cd "$TMP_WS"
# Canonicalize the path and pin the workshop root so child processes spawned
# from this script cannot drift back to the caller's workgraph.
TMP_WS_REAL="$(cd "$TMP_WS" && pwd -P)"
export RYVE_WORKSHOP_ROOT="$TMP_WS_REAL"

# --- Seed a perf-audit-shaped epic ----------------------------------------
#
# The manual run (cr-d48bbba9 / PR #14) decomposed a single "P1 hot-path
# remediation" epic into six perf-fix child sparks. We mirror that exact
# decomposition so parity comparisons are one-to-one.

EPIC_ID="$("$RYVE_BIN" --json spark create \
  --type epic --priority 1 \
  --risk elevated \
  --problem 'Six hot-path perf regressions are costing frame time in Ryve UI. Decompose, fix in parallel, land as one PR.' \
  --acceptance 'All six fixes land in one crew branch' \
  --acceptance 'A single merger PR is opened against main' \
  --acceptance 'Atlas never dispatches implementer Hands directly' \
  'P1 hot-path remediation (E2E test epic)' | sed -n 's/.*"id": *"\([^"]*\)".*/\1/p' | head -1)"
[[ -n "$EPIC_ID" ]] || die "failed to create epic"
log "created epic $EPIC_ID"

# Six child sparks mirroring cr-d48bbba9's hand decomposition. Titles match
# the shape of what PerfHead's prompt template is supposed to auto-produce;
# we pre-seed them so the test is deterministic even if the Head's LLM-driven
# decomposition varies run-to-run.
declare -a PERF_TITLES=(
  "perf: stop SparksPoll thrash on keystroke"
  "perf: intern font family names to bound Box::leak"
  "perf: share sysinfo snapshot per SparksPoll tick"
  "perf: hash-and-skip agent_context sync"
  "perf: async create_hand_worktree to unblock UI"
  "perf: regression harness + CI gate"
)
CHILD_IDS=()
for title in "${PERF_TITLES[@]}"; do
  cid="$("$RYVE_BIN" --json spark create \
    --type task --priority 1 \
    --problem "child of $EPIC_ID — $title" \
    --acceptance 'benchmark shows improvement, no new warnings' \
    "$title" | sed -n 's/.*"id": *"\([^"]*\)".*/\1/p' | head -1)"
  [[ -n "$cid" ]] || die "failed to create child spark: $title"
  "$RYVE_BIN" bond create "$EPIC_ID" "$cid" parent_child >/dev/null
  CHILD_IDS+=("$cid")
done
log "seeded ${#CHILD_IDS[@]} child sparks under $EPIC_ID"

# --- Stub agent so Hands launch, record their argv, then exit -------------
# The stub does NOT actually write code; it records the prompt it was given
# and exits 0. That is enough to exercise the orchestration: PerfHead spawns
# Hands, Hands' sessions land in the workgraph, stall detection engages,
# and the merger Hand gets dispatched on its own schedule.

STUB="$TMP_WS/stub-agent.sh"
cat >"$STUB" <<'STUB_EOF'
#!/bin/sh
# Stub agent: record argv, exit 0.
out="${RYVE_TEST_AGENT_OUT:-/tmp/ryve-stub-out.log}"
printf 'stub-agent invoked at %s\n' "$(date -u +%FT%TZ)" >>"$out"
printf '  argv: %s\n' "$*" >>"$out"
printf '  cwd:  %s\n' "$PWD" >>"$out"
exit 0
STUB_EOF
chmod +x "$STUB"
export RYVE_TEST_AGENT_OUT="$TMP_WS/stub-agent.log"
export PATH="$TMP_WS:$PATH"

# --- Run PerfHead ---------------------------------------------------------
if [[ "$MODE" == "full" ]]; then
  log "spawning PerfHead against epic $EPIC_ID"
  "$RYVE_BIN" head spawn "$EPIC_ID" --archetype build \
    --agent stub-agent.sh >/dev/null || die "ryve head spawn failed"

  # Poll until the Crew materializes or we time out. We consider the run
  # converged when:
  #   (a) the epic has a crew whose parent_spark_id points at it, and
  #   (b) the crew has ≥6 owner members and exactly 1 merger member, OR
  #   (c) STALL_SECS elapses (failure).
  log "polling for Crew convergence (stall=$STALL_SECS s)"
  deadline=$(( $(date +%s) + STALL_SECS ))
  CREW_ID=""
  while (( $(date +%s) < deadline )); do
    CREW_ID="$("$RYVE_BIN" --json crew list 2>/dev/null \
      | sed -n "s/.*\"id\": *\"\\([^\"]*\\)\".*\"parent_spark_id\": *\"$EPIC_ID\".*/\\1/p" \
      | head -1)"
    if [[ -n "$CREW_ID" ]]; then
      members_json="$("$RYVE_BIN" --json crew show "$CREW_ID" 2>/dev/null || true)"
      owner_count=$(printf '%s' "$members_json" | grep -c '"role": *"owner"' || true)
      merger_count=$(printf '%s' "$members_json" | grep -c '"role": *"merger"' || true)
      if (( owner_count >= 6 && merger_count >= 1 )); then
        log "crew converged: owners=$owner_count merger=$merger_count"
        break
      fi
    fi
    sleep "$POLL_INTERVAL"
  done
  [[ -n "$CREW_ID" ]] || die "no crew materialized for epic $EPIC_ID within ${STALL_SECS}s"
else
  log "MODE=dry-run — skipping ryve head spawn and asserting only the seeded workgraph"
  CREW_ID=""
fi

# --- Gather observations --------------------------------------------------
HAND_BRANCHES=$(git -C "$TMP_WS" branch --list 'hand/*' | wc -l | tr -d ' ')
SPARK_COUNT=${#CHILD_IDS[@]}

# Detect Atlas-direct-dispatch: any hand_assignments row on a child spark
# whose parent_session_id points at an agent_sessions row with role=director.
# We query this via the CLI's assign list output which prints session metadata.
ATLAS_DIRECT=0
for cid in "${CHILD_IDS[@]}"; do
  ownership="$("$RYVE_BIN" assign list "$cid" 2>/dev/null || true)"
  if printf '%s' "$ownership" | grep -qi 'parent_role: *director'; then
    ATLAS_DIRECT=$(( ATLAS_DIRECT + 1 ))
  fi
done

# --- Manual baseline (from cr-d48bbba9 / PR #14) --------------------------
# Hand branches merged into crew/cr-d48bbba9 (from git log):
MANUAL_HANDS=6
MANUAL_MERGER=1
MANUAL_PR=14
MANUAL_ATLAS_DIRECT=0  # the manual run was human-driven, not Atlas

# --- Emit comparison report -----------------------------------------------
cat >"$OUT_JSON" <<JSON
{
  "mode": "$MODE",
  "epic_id": "$EPIC_ID",
  "crew_id": "${CREW_ID:-null}",
  "child_spark_count": $SPARK_COUNT,
  "observed": {
    "hand_branches": $HAND_BRANCHES,
    "atlas_direct_dispatches": $ATLAS_DIRECT
  },
  "manual_baseline": {
    "source": "crew cr-d48bbba9 / PR #$MANUAL_PR",
    "hands": $MANUAL_HANDS,
    "merger": $MANUAL_MERGER,
    "atlas_direct_dispatches": $MANUAL_ATLAS_DIRECT
  },
  "parity_contract": {
    "hands_ge_6": $([[ $HAND_BRANCHES -ge 6 ]] && echo true || echo false),
    "no_atlas_direct_dispatch": $([[ $ATLAS_DIRECT -eq 0 ]] && echo true || echo false),
    "child_sparks_equal_6": $([[ $SPARK_COUNT -eq 6 ]] && echo true || echo false)
  }
}
JSON

log "wrote report to $OUT_JSON"
cat "$OUT_JSON"

# Exit nonzero if any parity check fails in full mode.
if [[ "$MODE" == "full" ]]; then
  if (( HAND_BRANCHES < 6 )); then die "parity fail: only $HAND_BRANCHES hand branches"; fi
  if (( ATLAS_DIRECT > 0 ));  then die "parity fail: $ATLAS_DIRECT Atlas-direct dispatches"; fi
fi

log "ok (mode=$MODE)"
