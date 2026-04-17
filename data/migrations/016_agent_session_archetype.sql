-- Persist the Hand archetype id on agent_sessions so the UI + replay can
-- surface which specialized role (Noop, Cartographer, Bug Hunter, …) a
-- Hand was booted under. See `src/hand_archetypes.rs` for the registry.
--
-- NULL means the session was spawned without an archetype — either the
-- row predates this column (older workshops) or the caller is the
-- legacy Owner/Merger/Head/Investigator path that selects its flavour
-- via `session_label` instead. Back-compat loading is exercised by the
-- `spawn_hand` tests in `src/hand_spawn.rs`.
ALTER TABLE agent_sessions ADD COLUMN archetype_id TEXT;
