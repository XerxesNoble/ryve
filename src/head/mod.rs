// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Head-side primitives. A Head is a coding-agent subprocess that
//! orchestrates a Crew of Hands; this module gives Heads a reusable
//! in-process orchestration loop so the decomposition → fan-out → poll →
//! reassign → finalize policy does not have to be re-implemented per
//! archetype prompt.
//!
//! See `orchestrator` for the actual loop primitives.

pub mod orchestrator;
