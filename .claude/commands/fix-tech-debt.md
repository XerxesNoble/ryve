Fix a tech debt item from `docs/TECH_DEBT.md`. Follow this workflow:

1. Read `docs/TECH_DEBT.md` and list all open items with their IDs, priorities, and effort levels
2. Ask which item to fix (or pick the highest-priority, lowest-effort item if told to choose)
3. Create a tracking spark: `./target/debug/ryve spark create --type task --priority 2 --problem "<description of the debt>" --acceptance "all instances of the violation removed across the workspace" "Fix TD-NNN: <short description>"` and note the returned spark id
4. Mark it in progress: `./target/debug/ryve spark status <spark-id> in_progress`
5. Read the relevant source files identified in the tech debt entry
6. Fix all instances of the violation across the codebase
7. If the fix involves changing a lint level in `Cargo.toml`, update it (e.g., `"allow"` → `"warn"`)
8. Run `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` to verify the fix
9. Update `docs/TECH_DEBT.md` to mark the item as resolved (remove or mark done)
10. Close the spark: `./target/debug/ryve spark close <spark-id> completed` and add a closing comment with `./target/debug/ryve comment add <spark-id> "Fixed N instances across M files"`
11. Summarize what was changed and how many instances were fixed

$ARGUMENTS
