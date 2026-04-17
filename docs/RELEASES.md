# Releases

A **Release** in Ryve is the unit that turns a set of completed epics into a
versioned, tagged, built artifact. It is the seam between the workgraph
(sparks, bonds, crews) and the outside world — a release is what gets
shipped, archived, and pointed at.

This document is the canonical reference for Releases: the concept, the
lifecycle states, the CLI surface, the branch/version conventions, and the
**Release Manager** archetype that drives the close ceremony on Atlas's
behalf. Architectural rationale and per-module deep dives live in the
source files referenced below.

## Concept

A Release bundles three things:

1. A **strict semver version** (`MAJOR.MINOR.PATCH`) that uniquely names
   the release.
2. A **dedicated git branch** (`release/<version>`) cut from `main` at
   creation time. The branch is the staging area for any last-mile fixes
   on the way to the tag.
3. A **set of member epics** — closed `epic`-typed sparks whose work the
   release ships. The release row stores membership in `release_epics`;
   the gate at close time refuses any release whose member epics are not
   all closed.

When a release is **closed**, three additional artifacts come into being:

- An annotated git tag `v<version>` on the release branch HEAD.
- A built binary at the deterministic path
  `.ryve/releases/<version>/ryve-<version>-<host-target-triple>`.
- A `releases` row in `closed` state with the tag name and artifact path
  persisted, so the UI and external automation can recover both pointers
  from a single SQL query.

There is **at most one open release at any time** — `Planning`,
`InProgress`, and `Ready` are all "open" states and a member epic can only
belong to one open release. The `release_epics_single_open_insert`
trigger enforces this in the database; the data layer surfaces it as
`SparksError::EpicAlreadyInOpenRelease`. Stress-testing concurrent
releases is an explicit non-goal of the v1 epic.

## Lifecycle states

The `releases.status` column stores one of six states. The state machine
is enforced at the data layer
([`data/src/sparks/types.rs`](../data/src/sparks/types.rs)):

| State         | Meaning                                                                  | Open? |
|---------------|--------------------------------------------------------------------------|:-----:|
| `planning`    | Created. Branch cut. Membership being assembled.                         |   ✓   |
| `in_progress` | Member epics are being worked.                                           |   ✓   |
| `ready`       | All member epics closed; release is ready for the close ceremony.        |   ✓   |
| `cut`         | Release branch frozen. `cut_at` timestamp stamped on transition.         |       |
| `closed`      | Tag created, artifact built, metadata persisted. Terminal success state. |       |
| `abandoned`   | Release was given up on. Terminal failure state.                         |       |

`is_open()` returns true for `planning | in_progress | ready` — those are
the states that block another release from claiming the same epic.

Transitions are linear in normal use:

```
   planning → in_progress → ready → cut → closed
                                       ↓
                                  (abandoned)
```

`cut_at` is stamped automatically by `release_repo::set_status` the first
time a release transitions into `Cut`. `tag` and `artifact_path` are
written by `release_repo::record_close_metadata` during the close
ceremony, before the final `Closed` transition.

## CLI commands

All release operations go through the `ryve release` subcommand. The full
implementation lives in [`src/cli.rs`](../src/cli.rs) under
`handle_release`; the close ceremony is `release_close` in the same file.

```sh
ryve release create <major|minor|patch>
    Create a new release. Computes the next version from the highest
    closed release (or 0.0.0 if none), writes the row, and cuts
    `release/<version>` from `main` in the same step. Refuses if the
    working tree is dirty.

ryve release list
    Print every release with its id, version, status, and branch.

ryve release show <release_id>
    Print release details + the list of member epic ids. Add `--json`
    for a machine-readable payload.

ryve release edit <release_id>
        [--version <semver>]
        [--notes <text> | --clear-notes]
        [--problem <text> | --clear-problem]
    Mutate fields on the release row in place. Used to fix up the
    version or notes before close.

ryve release add-epic <release_id> <epic_id>
ryve release remove-epic <release_id> <epic_id>
    Manage release membership. The data layer rejects adding an epic
    that already belongs to another open release.

ryve release status <release_id> <new_status>
    Drive a manual status transition. Mostly an escape hatch — the
    canonical close path is `release close`, which transitions through
    `closed` itself.

ryve release close <release_id>
    Run the full close ceremony (see below).
```

Every mutating command supports `--json` for scripting; pass it before
the subcommand: `ryve --json release create patch`.

### The close ceremony

`ryve release close <id>` is the single atomic operation that takes a
release from any open state to `closed`. The orchestration lives in
[`src/cli.rs::release_close`](../src/cli.rs); the steps are:

1. **Verify** every member epic is closed. Any unclosed epic aborts the
   close with a non-zero exit and the release stays in its current state.
2. **Checkout** the release branch (the original branch is restored on
   exit so the working tree returns to where the operator started).
3. **Tag** the branch HEAD as `v<version>`. The tag message records the
   anticipated artifact path. Refuses on a dirty tree, wrong branch, or
   detached/diverged HEAD ([`data/src/release_branch.rs`](../data/src/release_branch.rs)).
4. **Build** the release artifact via `cargo build --release` and stage it
   at the deterministic path
   `.ryve/releases/<version>/ryve-<version>-<host-triple>`
   ([`src/release_artifact.rs`](../src/release_artifact.rs)).
5. **Record** the tag and artifact path on the release row.
6. **Transition** the row to `closed`.

Failures in steps 3–6 trigger best-effort rollback (delete the tag,
remove the artifact, clear the metadata) so the database never observes a
half-closed release.

## Branch naming

Every release owns exactly one git branch named `release/<version>`. The
prefix is the constant `RELEASE_BRANCH_PREFIX` in
[`data/src/release_branch.rs`](../data/src/release_branch.rs); no other
call site is allowed to mutate `release/*` branches (cherry-picking or
hand-merging into a release branch is out of scope).

The release branch is **cut from `main`** at the same moment the
`releases` row is created. The branch and the row are produced atomically
from the CLI's perspective — a failure to cut the branch is reported as a
warning alongside the row, and the release is left in `planning` for the
operator to retry the cut manually.

The `pre_merge_validator` ([`data/src/pre_merge_validator.rs`](../data/src/pre_merge_validator.rs))
recognises `release/<v>` → `main` as the Release Manager's legal landing
path. Other branches (`hand/<id>`, `epic/<id>`, `crew/<id>`) have their
own legal targets and the validator refuses cross-class merges.

## Semver rules

Release versions are **strict** `MAJOR.MINOR.PATCH`. The single source of
truth is [`data/src/release_version.rs`](../data/src/release_version.rs):

- Three decimal components, separated by `.`. No pre-release suffixes
  (`-rc1`, `-beta`), no build metadata (`+sha`), no leading `v`, no
  surrounding whitespace, no leading zeros on multi-digit components
  (`01.2.3` is rejected).
- Bumps are computed from the highest **closed** release's version (open
  releases are excluded from the baseline). The first-ever release bumps
  from the implicit baseline `0.0.0`:
  - `next(None, Major) == 1.0.0`
  - `next(None, Minor) == 0.1.0`
  - `next(None, Patch) == 0.0.1`
- Bumps **strictly advance**. A bump that would not produce a strictly
  greater version (the saturating-overflow corner) is rejected as
  `VersionError::Downgrade`.

`release/<version>` and `v<version>` both use the same canonical string;
the artifact filename embeds it verbatim.

## Release Manager archetype

The **Release Manager** is the Hand archetype that owns a release through
its lifecycle. Its job is mechanically narrow: drive `ryve release *`
subcommands, commit on the `release/<version>` branch, and report status
to Atlas. Nothing else.

### Atlas-only communication discipline

The Release Manager has a **deliberately narrow communication graph**:

- It takes direction **only from Atlas**. Atlas decides which epics are
  in-scope and when to start the close ceremony; the Release Manager
  executes.
- It reports **only to Atlas**. Comments, status updates, and questions
  go to Atlas, never to other Hands or Heads. Atlas synthesises Release
  Manager output back to the user.
- It cannot spawn Hands or Heads. It cannot send embers to peers. It
  cannot edit sparks outside the release it manages.

This discipline is mechanical, not advisory: the archetype boots with a
`ToolPolicy` allow-list of `ryve release *`, `ryve comment add` targeted
at Atlas (and at release member sparks), read-only workgraph queries,
and `git` operations on `release/*` branches. Any other action is
rejected with a typed `PolicyError` before the database is touched.

The narrowness is the point: a Release Manager that can only talk to
Atlas cannot accidentally fan out releases to multiple stakeholders, and
cannot drift mid-ceremony into unrelated work. Atlas remains the single
user-facing voice for everything release-related.

### Singleton

There is **at most one Release Manager active at a time**, mirroring the
"at most one open release" invariant. Multiple Release Managers running
in parallel is an explicit non-goal of the epic.

### Where the archetype lives in code

The archetype is registered as a `HandKind` variant in
[`src/hand_spawn.rs`](../src/hand_spawn.rs) and its prompt is composed
in [`src/agent_prompts.rs`](../src/agent_prompts.rs), alongside the
other Hand archetypes (Owner, Investigator, Merger). The CLI surface is
`ryve hand spawn <release_spark> --role release_manager`. The tool-policy
allow-list that mechanically enforces the Atlas-only discipline lives in
the same registry as the other archetypes — adding new gated actions
means extending that registry, not editing per-archetype branches.

The UI nudges users toward Atlas rather than spawning a Release Manager
directly — when the operator clicks **Request close** in the Releases
panel ([`src/screen/releases.rs`](../src/screen/releases.rs)), the
workshop emits a toast saying "Ask Atlas to spawn a Release Manager to
close release <id>." The close flow is owned by the archetype, not by
the UI's update loop ([`src/app.rs`](../src/app.rs)).

## Example workflow

A complete release cycle, from planning through closed:

```sh
# 1. Operator (via Atlas) starts a new patch release. The CLI computes
#    the next version, writes the releases row, and cuts the branch.
ryve release create patch
# → created rel-0414d7ec — v0.0.1 (planning)
#     branch: release/0.0.1

# 2. Atlas (or the operator) tells the Release Manager which epics are
#    in scope. Membership is just a join row — no commits required.
ryve release add-epic rel-0414d7ec ryve-aaaa1111
ryve release add-epic rel-0414d7ec ryve-bbbb2222

# 3. The Crew Heads working those epics finish their work, the Mergers
#    land their PRs, and the epics close in the normal way:
ryve spark close ryve-aaaa1111 completed
ryve spark close ryve-bbbb2222 completed

# 4. Optional: walk the release through `in_progress` → `ready` so the
#    UI shows progress. Both transitions are pure state changes.
ryve release status rel-0414d7ec in_progress
ryve release status rel-0414d7ec ready

# 5. Atlas spawns a Release Manager Hand on the release. The RM is
#    mechanically restricted to release operations + Atlas comms.
ryve hand spawn rel-0414d7ec --role release_manager --agent claude

# 6. The Release Manager runs the close ceremony. This is the single
#    atomic step that tags, builds, records, and transitions to closed.
ryve release close rel-0414d7ec
# → release rel-0414d7ec closed
#     tag:      v0.0.1
#     artifact: /path/to/workshop/.ryve/releases/0.0.1/ryve-0.0.1-aarch64-apple-darwin

# 7. Inspect the closed release. The row carries pointers to both the
#    tag and the artifact, so any downstream automation has a single
#    source of truth.
ryve --json release show rel-0414d7ec
```

The end-to-end behaviour above is exercised by
[`tests/release_e2e.rs`](../tests/release_e2e.rs), which runs against a
self-contained temp workshop with a fixture cargo project so the real
tree is never touched.

## See also

- [`docs/AGENT_HIERARCHY.md`](AGENT_HIERARCHY.md) — Atlas / Head / Hand
  layering that the Release Manager fits into.
- [`docs/HEAD_ARCHETYPES.md`](HEAD_ARCHETYPES.md) — Head archetypes
  (Build / Research / Review). The Release Manager is a **Hand**
  archetype, not a Head; it drives one release rather than a crew.
- [`data/src/release_branch.rs`](../data/src/release_branch.rs) — git
  invariants for `release/*` branches.
- [`data/src/release_version.rs`](../data/src/release_version.rs) —
  semver parsing, formatting, and bump math.
- [`data/src/sparks/release_repo.rs`](../data/src/sparks/release_repo.rs) —
  data-layer CRUD, status transitions, membership invariants.
- [`src/release_artifact.rs`](../src/release_artifact.rs) —
  artifact-build pipeline + deterministic-path contract.
