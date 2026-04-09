# DONE Checklist

A spark is only "done" when ALL of the following are true. Verify each item
before closing the spark with `ryve-cli spark close <id>`.

## Code
- [ ] All acceptance criteria from the spark intent are satisfied
- [ ] Code compiles cleanly (no new warnings introduced)
- [ ] No `todo!()`, `unimplemented!()`, or stub functions left behind
- [ ] No debug prints, `dbg!`, or commented-out code
- [ ] No `#[allow(...)]` attributes added to suppress clippy or compiler warnings — fix the code instead

## Tests
- [ ] New behavior has at least one test (unit or integration)
- [ ] All existing tests still pass
- [ ] Edge cases identified in the spark are covered

## Workgraph hygiene
- [ ] Commit messages reference the spark id: `[sp-xxxx]`
- [ ] Any new bugs/tasks discovered were created as new sparks
- [ ] All required contracts on the spark pass (`ryve-cli contract list <id>`)
- [ ] Architectural constraints respected (`ryve-cli constraint list`)

## Done
- [ ] Spark closed: `ryve-cli spark close <id> completed`
