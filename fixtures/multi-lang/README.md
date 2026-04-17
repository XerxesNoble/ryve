# Multi-Language Fixture Project

Synthetic project used by `tests/archetype_language_agnostic.rs` to prove
that Ryve's Hand archetypes do not assume the user works in any specific
language — Rust included. The tree contains Python, TypeScript, and Go
sources only; there are deliberately zero Rust files anywhere under
`fixtures/multi-lang/`.

## Fake spec

`OrderRouter` is a pretend service that fans incoming order events out to
three downstream workers, each implemented in a different language so the
fixture exercises a polyglot codebase:

- `app.py` — the Python ingestion handler.
- `app.ts` — the TypeScript validation worker.
- `app.go` — the Go fan-out scheduler.

## Goals

The integration test asks: when an archetype Hand is spawned against a
spark whose scope points at this fixture, does its rendered prompt avoid
any tokens that would steer the Hand toward Rust ecosystem conventions
(`cargo`, `clippy`, `rustc`, `rustup`, `rustfmt`, `Cargo.toml`,
`crates.io`, `mod.rs`, `rust-toolchain`, `rust-analyzer`)? And does it
preserve the read-vs-write tool discipline its archetype is supposed to
enforce?

A failing assertion blocks the parent epic — Hand archetypes that bake
in Rust assumptions are a regression on the polyglot invariant.
