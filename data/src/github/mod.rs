// SPDX-License-Identifier: AGPL-3.0-or-later

//! GitHub Issues sync for Workgraph.

pub mod applier;
pub mod sync;
pub mod translator;
pub mod types;

pub use applier::{
    APPLIER_SCHEMA_VERSION, AppliedOutcome, ApplyError, EVT_ARTIFACT_RECORDED,
    EVT_ILLEGAL_TRANSITION_WARNING, EVT_ORPHAN_EVENT_WARNING, EVT_PHASE_TRANSITIONED,
    GithubEventsSeenRepo, apply,
};
pub use sync::GitHubSync;
pub use translator::{GitHubPayload, TranslateError, translate};
pub use types::{CanonicalGitHubEvent, GitHubArtifactRef};
