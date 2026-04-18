// SPDX-License-Identifier: AGPL-3.0-or-later

//! GitHub Issues sync for Workgraph.

pub mod applier;
pub mod orphan_scan;
pub mod poller;
pub mod rate_limit;
pub mod sync;
pub mod translator;
pub mod types;

pub use applier::{
    APPLIER_SCHEMA_VERSION, AppliedOutcome, ApplyError, EVT_ARTIFACT_RECORDED,
    EVT_ILLEGAL_TRANSITION_WARNING, EVT_ORPHAN_EVENT_WARNING, EVT_PHASE_TRANSITIONED,
    GithubEventsSeenRepo, apply,
};
pub use orphan_scan::{
    DEFAULT_DEBOUNCE_SECONDS, EVT_ORPHAN_ASSIGNMENT_WARNING, ORPHAN_SCAN_ACTOR,
    ORPHAN_SCAN_EVENT_TYPE, OrphanScanOutcome, is_orphan_candidate, run_orphan_scan,
    run_orphan_scan_with,
};
pub use poller::{
    DEFAULT_POLL_CADENCE, FetchResponse, FetchedEvent, PollError, Poller, PollerConfig,
    TickOutcome, current_epoch, default_jitter, run_forever,
};
pub use rate_limit::{
    BackoffReason, DEFAULT_BACKOFF_BASE, DEFAULT_BACKOFF_CAP, ExponentialBackoff, RateLimitInfo,
    ResponseOutcome, classify,
};
pub use sync::GitHubSync;
pub use translator::{GitHubPayload, TranslateError, translate};
pub use types::{CanonicalGitHubEvent, GitHubArtifactRef};
