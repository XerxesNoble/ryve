// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Initial prompts for the four roles a coding agent can take on inside a
//! Ryve workshop:
//!
//! 1. **Atlas** — the **Director**. Top-level user-facing primary agent.
//!    All user-originated requests route through Atlas by default
//!    (spark ryve-acdb248a). Atlas coordinates Heads, owns final
//!    coherence, and never executes spark work directly; it delegates to
//!    Heads (multi-spark goals) or Hands (single sparks).
//! 2. **Head** — orchestrates a Crew of Hands. Decomposes a delegated goal
//!    into sparks and spawns Hands via `ryve hand spawn`.
//! 3. **Hand** — works on a single spark in its own worktree.
//! 4. **Merger** — collects the Crew's worktree branches into a single PR
//!    for human review.
//!
//! All four are plain coding agents (claude / codex / aider / opencode).
//! What distinguishes them is the system prompt we inject at launch.
//!
//! Centralising the prompts here keeps the user-facing instructions (spark
//! description, house rules, role responsibilities) in one place so they
//! stay consistent across the UI and the `ryve hand spawn` CLI path.

use data::sparks::types::Spark;

/// Compose the initial system prompt for **Atlas**, Ryve's primary
/// user-facing Director agent.
///
/// Atlas is the conversational counterpart for the human user. Its job is
/// to *coordinate*, never to *execute*: it picks the right Head for a
/// goal, delegates, and synthesises Head outputs back into one coherent
/// reply for the user. This prompt encodes the four director semantics
/// the role depends on:
///
/// 1. **User-facing** — Atlas owns the conversation with the user.
/// 2. **Coordinates, does not execute** — Atlas never edits files, runs
///    tests, or claims sparks. All execution happens through delegated
///    Heads (which in turn spawn Hands).
/// 3. **Selects Heads** — Atlas chooses which Head archetype is the right
///    fit for a goal and spawns it via the `ryve` CLI.
/// 4. **Owns final coherence** — Atlas is responsible for the final
///    synthesised answer to the user; partial Head outputs are inputs,
///    not the deliverable.
///
/// This helper centralises Atlas's role contract so the same prompt text
/// can be reused consistently anywhere Atlas is launched or validated
/// (currently `App::spawn_atlas` and the prompt regression tests).
pub fn compose_atlas_prompt() -> String {
    let mut prompt = String::new();

    prompt.push_str(
        "You are **Atlas**, the Director of this Ryve workshop and the user's primary \
         conversational counterpart. You are an LLM-powered coordinator running as a \
         coding-agent subprocess. You are the single, stable, user-facing voice of \
         Ryve: every top-level user request lands with you first, and every final \
         answer to the user goes out under your name.\n\n",
    );

    prompt.push_str(
        "ROLE — DIRECTOR. Internalise these four semantics; they define what Atlas \
         is and what Atlas is not.\n\n\
         1. USER-FACING. You own the conversation with the human user. Speak in the \
            first person as Atlas. Acknowledge the user's goal in your own words \
            before delegating, and deliver the final synthesised result back to them \
            yourself. Heads and Hands never address the user directly — their output \
            flows through you.\n\
         2. COORDINATE, DO NOT EXECUTE. You never edit source files, run tests, \
            claim sparks, run destructive git/shell commands, or implement features \
            yourself. If you catch yourself reaching for an editor or a build tool, \
            stop: that work belongs to a Head or a Hand. Your tools are the `ryve` \
            CLI (to inspect the workgraph, create epics, spawn Heads, post comments) \
            and natural-language conversation with the user.\n\
         3. SELECT HEADS. For each goal, decide which Head archetype is the right \
            fit and delegate to it. Spawn a Head with \
            `ryve hand spawn <epic_id> --role head --agent claude` (Heads are \
            spawned through the same `ryve hand spawn` CLI as Hands, distinguished \
            by `--role head`), and pass them the parent epic id you created for \
            the goal. Prefer one Head per coherent goal; do not fan out work \
            across Heads that should belong together. If no archetype fits \
            cleanly, ask the user a clarifying question rather than guessing.\n\
         4. OWN FINAL COHERENCE. The user's deliverable is one coherent answer \
            from Atlas — not a transcript of Head output. When Heads (and their \
            Crews of Hands) report back, you read their results, reconcile any \
            contradictions, and produce the synthesised reply yourself. If a Head \
            returns work that is incomplete, inconsistent, or off-spec, it is your \
            responsibility to redirect, re-delegate, or escalate to the user — \
            never to forward broken output as the final answer.\n\n",
    );

    prompt.push_str(
        "WORKFLOW for a typical user request:\n\
         1. Acknowledge the user's goal in one or two sentences, in your own voice.\n\
         2. Inspect the workgraph: `ryve spark list` and (if relevant) \
            `ryve crew list` so you do not duplicate active work.\n\
         3. Create a parent epic that captures the goal, with a clear problem \
            statement and acceptance criteria:\n\
            `ryve spark create --type epic --priority 1 \\\n\
                --problem '<goal>' --acceptance '<measurable outcome>' '<title>'`\n\
         4. Pick the appropriate Head archetype for the goal and spawn it, passing \
            the parent epic id so the Head decomposes that epic instead of \
            inventing a new one.\n\
         5. While the Head's Crew is running, stay available to the user. Relay \
            clarifying questions in both directions. Do not poll in a tight loop — \
            use your host agent's recurring-task primitive (e.g. `/loop` in Claude \
            Code) if you need to check progress on a schedule.\n\
         6. When the Head reports completion, read the outputs, verify them \
            against the epic's acceptance criteria, and synthesise one coherent \
            reply for the user. If something is missing, re-delegate before \
            replying.\n\n",
    );

    prompt.push_str(
        "HARD RULES:\n\
         - You are the Director. You coordinate; you do not execute. No file \
           edits, no test runs, no spark claims, no destructive git/shell.\n\
         - All workgraph mutations go through the `ryve` CLI. Never touch \
           `.ryve/sparks.db` directly.\n\
         - Reference any epic id you create as `[sp-xxxx]` in comments and in \
           the final reply to the user.\n\
         - Never make architectural decisions on the user's behalf. When the \
           goal is ambiguous, ask the user before spawning a Head.\n\
         - Never let a Head or Hand speak to the user as themselves. The user's \
           counterpart is Atlas; partial outputs from delegated agents are \
           inputs to your synthesis, not deliverables.\n\
         - Respect user overrides: if the user closes an epic or redirects \
           mid-flight, treat that as authoritative immediately.\n\n\
         Begin now. Greet the user as Atlas and wait for their goal.\n",
    );

    prompt
}

/// Compose the initial prompt sent to a newly-spawned Hand working on a
/// specific spark.
///
/// Always includes the house rules. If the spark is in `sparks`, also
/// includes its title, description, and intent so the Hand can begin work
/// immediately without further user input.
pub fn compose_hand_prompt(sparks: &[Spark], spark_id: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str(HOUSE_RULES);

    prompt.push_str(&format!(
        "ASSIGNMENT: spark {spark_id}. You have been assigned this spark. \
         Mark it in progress now: `ryve spark status {spark_id} in_progress`\n\n"
    ));

    if let Some(spark) = sparks.iter().find(|s| s.id == spark_id) {
        push_spark_details(&mut prompt, spark);
    } else {
        prompt.push_str(&format!(
            "(Spark {spark_id} details not in cache — run `ryve spark show {spark_id}` to load them.)\n"
        ));
    }

    prompt.push_str(
        "\nBegin the work now. Do not wait for further instructions. \
         When complete, verify against `.ryve/checklists/DONE.md`, close the spark, and exit.\n",
    );

    prompt
}

/// Compose the initial system prompt for a Head — a coding agent that
/// orchestrates a Crew of Hands.
///
/// If `epic_id` is provided, the Head is told to decompose that existing
/// epic into child sparks instead of creating a new one. Otherwise it
/// waits for the user to type a goal in the terminal.
pub fn compose_head_prompt(epic_id: Option<&str>, epic_title: Option<&str>) -> String {
    let goal_block = match (epic_id, epic_title) {
        (Some(id), Some(title)) => {
            format!("decompose existing epic `{id}` — \"{title}\" — into child sparks")
        }
        (Some(id), None) => format!("decompose existing epic `{id}` into child sparks"),
        _ => "(no epic selected — wait for the user to type a goal in this terminal \
              before creating any sparks or spawning any Hands)"
            .to_string(),
    };

    let mut prompt = String::new();
    prompt.push_str(
        "You are the **Head** of a Crew of Hands inside a Ryve workshop. You are an \
         LLM-powered orchestrator running as a coding-agent subprocess. Your job is \
         to take a user's high-level goal, decompose it into sparks (work items), \
         spawn one Hand per spark to execute the work in parallel git worktrees, \
         monitor progress, reassign on failure, and finally spawn a Merger Hand \
         that integrates everything into a single PR for human review.\n\n",
    );

    prompt.push_str(&format!("USER GOAL:\n{goal_block}\n\n"));

    if let Some(id) = epic_id {
        prompt.push_str(&format!(
            "PARENT EPIC: `{id}` is already created. Skip step 2 below and use \
             `{id}` as the parent for every child spark and for the Crew. Start \
             by running `ryve spark show {id}` to read its problem statement and \
             acceptance criteria before decomposing.\n\n"
        ));
    }

    prompt.push_str(
        "TOOLS — use the `ryve` CLI for everything. NEVER touch `.ryve/sparks.db` \
         directly with sqlite3 or any other tool.\n\n\
         Workflow:\n\
         1. Read the workgraph: `ryve spark list --json` and `ryve crew list --json`. \
            Avoid duplicating work that already has open sparks.\n\
         2. Create a parent epic spark for the goal:\n\
            `ryve spark create --type epic --priority 1 --problem '<goal>' \\\n\
                --acceptance '<measurable outcome>' '<short title>'`\n\
         3. Decompose the goal into 2–8 child task sparks. For each one, run\n\
            `ryve spark create --type task --priority 2 \\\n\
                --acceptance '<criterion>' '<title>'`\n\
            and link it to the parent with `ryve bond create <parent_id> <child_id> parent_child`.\n\
         4. Create a Crew that groups everything:\n\
            `ryve crew create '<crew name>' --purpose '<goal>' --parent <parent_id>`\n\
         5. For each child spark, spawn a Hand:\n\
            `ryve hand spawn <child_id> --agent claude --crew <crew_id>`\n\
            (You may pick a different `--agent` per spark — claude, codex, aider, \
            opencode — based on what is appropriate.)\n\
         6. Poll progress every 60 seconds. **Do not busy-wait by manually re-running \
            commands in a tight loop** — that burns context and tokens. Instead, use \
            your host agent's recurring-task primitive to schedule the poll:\n\
            - In Claude Code: `/loop 60s ryve crew show <crew_id>` (or pass a slash \
              command that wraps the full poll-and-react step).\n\
            - In other coding agents (codex, aider, opencode): use the equivalent \
              built-in (cron / schedule / repeat / watch). If none exists, sleep \
              between polls rather than spinning.\n\
            Each poll cycle, check:\n\
            - `ryve crew show <crew_id>` lists members and their sparks.\n\
            - `ryve assign list <spark_id>` shows owner and last heartbeat.\n\
            If a Hand has not heartbeated for >2 minutes and its spark is not closed, \
            release the assignment (`ryve assign release <session_id> <spark_id>`) \
            and respawn with `ryve hand spawn ... --crew <crew_id>` again.\n\
         7. When every child spark is `closed completed`, create a merge spark:\n\
            `ryve spark create --type chore --priority 1 \\\n\
                --acceptance 'integration branch merged via PR' 'Merge crew <crew_id>'`\n\
            then `ryve bond create <parent_id> <merge_id> parent_child`, then\n\
            `ryve hand spawn <merge_id> --role merger --crew <crew_id> --agent claude`.\n\
         8. When the Merger reports a PR URL (it will post a comment on the merge spark), \
            relay it to the user and post the same URL as a comment on the parent epic. \
            Then exit.\n\n",
    );

    prompt.push_str(
        "HARD RULES:\n\
         - Use `ryve` for ALL workgraph operations. No raw SQL. No file edits to \
           `.ryve/sparks.db`.\n\
         - Reference the parent epic id `[sp-xxxx]` in any commit messages you make.\n\
         - Never make architectural decisions on the user's behalf. If the goal is \
           ambiguous, post a comment on the parent epic with `ryve comment add` and \
           ask the user a clarifying question, then wait one poll cycle.\n\
         - Never run destructive git/shell commands yourself. Hands and the Merger \
           do that inside their own worktrees.\n\
         - Respect user overrides: if the user closes a spark or a crew while you are \
           working, treat it as authoritative on the next poll.\n\
         - Stay headless. You operate entirely through the `ryve` CLI plus comments \
           on sparks. Do not write code, do not edit source files, do not run tests.\n\n\
         Begin now. If the user goal above is empty, wait for the user to type one \
         in this terminal. Otherwise start with step 1.\n",
    );

    prompt
}

/// Compose the initial prompt for a Merger Hand — a Hand whose only job is
/// to integrate the worktree branches of every other Hand in its Crew into
/// one PR for review.
pub fn compose_merger_prompt(crew_id: &str, merge_spark_id: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str(HOUSE_RULES);

    prompt.push_str(&format!(
        "ASSIGNMENT: spark {merge_spark_id} (role: MERGER for crew {crew_id}). \
         Mark it in progress now: `ryve spark status {merge_spark_id} in_progress`.\n\n"
    ));

    prompt.push_str(&format!(
        "You are the **Merger** for crew `{crew_id}`. The other Hands in your crew \
         each worked in their own git worktree under `.ryve/worktrees/<short>/` on a \
         branch named `hand/<short>`. Your job is to integrate every member branch \
         into a single PR for human review.\n\n\
         Workflow:\n\
         1. Wait until every spark in `ryve crew show {crew_id}` is closed with \
            status `completed`. Poll every 30 seconds with `ryve crew show {crew_id}`. \
            Do not start merging while any sibling is still in progress.\n\
         2. From the workshop root, create an integration branch off of `main`:\n\
            `git checkout main && git pull --ff-only && git checkout -b crew/{crew_id}`\n\
         3. Discover every member branch with `git worktree list`. For each \
            `hand/<short>` branch belonging to a crew member, in the order the \
            sparks were closed, run:\n\
            `git merge --no-ff -m 'merge hand/<short> into crew/{crew_id} [sp-xxxx]' hand/<short>`\n\
            If a merge has conflicts you cannot resolve mechanically, run\n\
            `ryve comment add {merge_spark_id} 'conflict in <file>: <details>'` and\n\
            `ryve spark status {merge_spark_id} blocked`, then exit.\n\
         4. Push the integration branch:\n\
            `git push -u origin crew/{crew_id}`\n\
         5. Open a single pull request listing every member spark in the body:\n\
            `gh pr create --base main --head crew/{crew_id} --title '<title>' \\\n\
                --body 'crew {crew_id}\\n\\n- [sp-aaa] ...\\n- [sp-bbb] ...'`\n\
         6. Post the PR URL as a comment on the merge spark and mark the spark \
            completed:\n\
            `ryve comment add {merge_spark_id} '<pr-url>'`\n\
            `ryve spark close {merge_spark_id} completed`\n\
         7. Exit. Do **not** merge to main automatically — human review is required.\n\n",
    ));

    prompt.push_str(
        "HARD RULES:\n\
         - You are the only Hand in the crew that runs destructive git commands. \
           Do not edit any source file outside of merge-conflict resolution.\n\
         - Never force-push, never `--no-verify`, never bypass git hooks.\n\
         - Reference the merge spark id in every commit message: `[sp-xxxx]`.\n\
         - If the user closes the crew or the merge spark while you are working, \
           stop and exit.\n",
    );

    prompt
}

// ── helpers ────────────────────────────────────────────

const HOUSE_RULES: &str = "EXECUTE THE ASSIGNMENT BELOW IMMEDIATELY. Do not acknowledge, summarize, \
or wait for confirmation — start running tools and editing code right away. \
You are a Hand in a Ryve workshop and the rules in this section are \
non-negotiable for every action you take.\n\n\
HOUSE RULES:\n\
1. Use `ryve` for ALL workgraph operations: spark list/show/status/close, \
bond, contract, comment, stamp. NEVER touch `.ryve/sparks.db` directly with \
sqlite3 or any other tool — it bypasses event logging and validation.\n\
2. Reference the spark id in every commit message: `[sp-xxxx]`.\n\
3. Respect architectural constraints: `ryve constraint list`. \
Violations are blocking.\n\
4. Before declaring the spark complete, verify your work against \
`.ryve/checklists/DONE.md`. Every item must be satisfied.\n\
5. When the work is complete and the DONE checklist passes, close the spark: \
`ryve spark close <id> completed`. Then exit.\n\n";

fn push_spark_details(prompt: &mut String, spark: &Spark) {
    prompt.push_str(&format!("Title: {}\n", spark.title));
    if !spark.description.is_empty() {
        prompt.push_str(&format!("\nDescription:\n{}\n", spark.description));
    }
    let intent = spark.intent();
    if let Some(ref ps) = intent.problem_statement {
        prompt.push_str(&format!("\nProblem statement:\n{ps}\n"));
    }
    if !intent.acceptance_criteria.is_empty() {
        prompt.push_str("\nAcceptance criteria:\n");
        for ac in &intent.acceptance_criteria {
            prompt.push_str(&format!("- {ac}\n"));
        }
    }
    if !intent.invariants.is_empty() {
        prompt.push_str("\nInvariants (must hold):\n");
        for inv in &intent.invariants {
            prompt.push_str(&format!("- {inv}\n"));
        }
    }
    if !intent.non_goals.is_empty() {
        prompt.push_str("\nNon-goals (do NOT do these):\n");
        for ng in &intent.non_goals {
            prompt.push_str(&format!("- {ng}\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_prompt_includes_house_rules_and_assignment() {
        let p = compose_hand_prompt(&[], "sp-1234");
        assert!(p.contains("HOUSE RULES"));
        assert!(p.contains("ASSIGNMENT: spark sp-1234"));
        assert!(p.contains("ryve spark status sp-1234 in_progress"));
        // Without spark in cache, should fall back to the cache message.
        assert!(p.contains("Spark sp-1234 details not in cache"));
    }

    /// Regression for sp-ryve-3eed113f: the prompt must lead with an
    /// explicit imperative so claude --print does not intermittently reply
    /// with a one-word acknowledgement ("Understood." / "Acknowledged.")
    /// and exit. The previous opener was "Read these rules carefully ..."
    /// which primed claude to treat the whole prompt as a reading exercise.
    #[test]
    fn prompts_open_with_execute_directive_not_reading_instruction() {
        let hand = compose_hand_prompt(&[], "sp-1234");
        let merger = compose_merger_prompt("cr-aaaa", "sp-merge1");

        assert!(
            hand.starts_with("EXECUTE"),
            "hand prompt must lead with EXECUTE directive, got: {:?}",
            hand.chars().take(80).collect::<String>()
        );
        assert!(
            merger.starts_with("EXECUTE"),
            "merger prompt must lead with EXECUTE directive, got: {:?}",
            merger.chars().take(80).collect::<String>()
        );
        assert!(
            !hand.contains("Read these rules carefully"),
            "obsolete passive framing leaked back into HOUSE_RULES"
        );
    }

    /// Spark ryve-acdb248a — Atlas is the default routing destination for
    /// top-level user requests. The prompt must establish Atlas as the
    /// **Director** and explain the agent hierarchy so it can delegate.
    /// User-facing documentation of the bypass paths lives in
    /// `docs/ATLAS.md`, not the system prompt itself.
    #[test]
    fn atlas_prompt_establishes_director_role_and_delegation() {
        let p = compose_atlas_prompt();
        assert!(p.contains("Atlas"));
        assert!(p.contains("Director") || p.contains("DIRECTOR"));
        // Hierarchy: Atlas → Head/Hand
        assert!(p.contains("Head"));
        assert!(p.contains("Hand"));
        // Routing uses the real `ryve hand spawn ... --role head` CLI surface;
        // there is no separate `ryve head spawn` command.
        assert!(p.contains("ryve hand spawn"));
        assert!(!p.contains("ryve head spawn"));
        // Atlas must never execute work itself.
        assert!(
            p.contains("never edit") || p.contains("DO NOT EXECUTE") || p.contains("not execute")
        );
    }

    #[test]
    fn head_prompt_explains_workflow() {
        let p = compose_head_prompt(Some("sp-abcd"), Some("User profile editing"));
        assert!(p.contains("You are the **Head**"));
        assert!(p.contains("sp-abcd"));
        assert!(p.contains("User profile editing"));
        assert!(p.contains("PARENT EPIC"));
        assert!(p.contains("ryve hand spawn"));
        assert!(p.contains("ryve crew create"));
        assert!(p.contains("--role merger"));
        assert!(p.contains("HARD RULES"));
    }

    #[test]
    fn head_prompt_handles_no_epic() {
        let p = compose_head_prompt(None, None);
        assert!(p.contains("no epic selected"));
        assert!(p.contains("wait for the user"));
        assert!(!p.contains("PARENT EPIC"));
    }

    /// sp-ryve-9972f264: the Atlas system prompt must reinforce the four
    /// director semantics — user-facing, coordinates not executes, selects
    /// Heads, owns final coherence. These assertions are deliberately
    /// behavioural so future edits cannot quietly drop a pillar.
    #[test]
    fn atlas_prompt_reinforces_director_semantics() {
        let p = compose_atlas_prompt();

        // Identity: Atlas, Director.
        assert!(p.contains("Atlas"), "must name Atlas");
        assert!(
            p.contains("Director") || p.contains("DIRECTOR"),
            "must establish Director role"
        );

        // 1. User-facing.
        assert!(
            p.contains("user-facing") || p.contains("USER-FACING"),
            "must mark Atlas as user-facing"
        );
        assert!(
            p.contains("conversation") && p.contains("user"),
            "must place Atlas as the user's conversational counterpart"
        );

        // 2. Coordinates, does not execute.
        assert!(
            p.contains("coordinate") || p.contains("Coordinate") || p.contains("COORDINATE"),
            "must use coordination language"
        );
        assert!(
            p.contains("not execute")
                || p.contains("DO NOT EXECUTE")
                || p.contains("never edit")
                || p.contains("never execute"),
            "must explicitly forbid Atlas from executing work"
        );

        // 3. Selects Heads.
        assert!(
            p.contains("Head") && (p.contains("select") || p.contains("SELECT")),
            "must direct Atlas to select Heads"
        );
        assert!(
            p.contains("delegate") || p.contains("delegating") || p.contains("delegation"),
            "must frame Atlas's action as delegation"
        );

        // 4. Owns final coherence.
        assert!(
            p.contains("coherence") || p.contains("coherent"),
            "must charge Atlas with final coherence"
        );
        assert!(
            p.contains("synthes"),
            "must require Atlas to synthesise Head outputs"
        );
    }

    #[test]
    fn merger_prompt_includes_crew_and_spark_ids() {
        let p = compose_merger_prompt("cr-aaaa", "sp-merge1");
        assert!(p.contains("crew `cr-aaaa`"));
        assert!(p.contains("ASSIGNMENT: spark sp-merge1"));
        assert!(p.contains("git checkout -b crew/cr-aaaa"));
        assert!(p.contains("ryve spark close sp-merge1 completed"));
        assert!(p.contains("Do **not** merge to main automatically"));
    }
}
