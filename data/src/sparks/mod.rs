// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Workgraph — Ryve's embedded issue tracker with dependency graph.

pub mod agent_session_repo;
pub mod alloy_repo;
pub mod assignment_repo;
pub mod bond_repo;
pub mod comment_repo;
pub mod commit_link_repo;
pub mod constraint_helpers;
pub mod contract_repo;
pub mod crew_repo;
pub mod ember_repo;
pub mod engraving_repo;
pub mod error;
pub mod event_repo;
pub mod file_link_repo;
pub mod graph;
pub mod id;
pub mod open_tab_repo;
pub mod spark_repo;
pub mod stamp_repo;
pub mod types;

pub use error::SparksError;
pub use types::*;
