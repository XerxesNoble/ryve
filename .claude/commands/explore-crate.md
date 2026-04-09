Deep-dive into a specific crate's architecture. For the crate specified below:

1. Read all `.rs` source files in the crate's `src/` directory
2. Map the public API: exported types, traits, functions, and constants
3. Identify the key code paths (e.g., main entry points, trait implementations, important state machines)
4. Document internal dependencies (what other crates/modules it imports)
5. Note any patterns specific to this crate (error types, builder patterns, async boundaries)
6. Produce a concise summary covering: purpose, public API surface, key internals, and dependencies

Target crate: $ARGUMENTS
