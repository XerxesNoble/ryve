// SPDX-License-Identifier: AGPL-3.0-or-later

//! Re-exports of panel state types used by [`crate::workshop::Workshop`].
//!
//! Workshop must not import from `crate::screen::*` — the screen modules
//! own rendering logic and iced widget trees while state types flow through
//! this façade. Screen modules remain the canonical definition site; this
//! module simply re-exports the subset Workshop needs.

pub mod agents {
    pub use crate::screen::agents::{AgentSession, AgentsPanelState};
}

pub mod background_picker {
    pub use crate::screen::background_picker::PickerState;
}

pub mod bench {
    pub use crate::screen::bench::{BenchState, TabKind};
}

pub mod file_explorer {
    pub use crate::screen::file_explorer::FileExplorerState;
}

pub mod file_viewer {
    pub use crate::screen::file_viewer::FileViewerState;
}

pub mod head_picker {
    pub use crate::screen::head_picker::PickerState;
}

pub mod intent_list_editor {
    pub use crate::screen::intent_list_editor::IntentListDrafts;
}

pub mod irc_view {
    pub use crate::screen::irc_view::IrcViewState;
}

pub mod log_tail {
    pub use crate::screen::log_tail::LogTailState;
}

pub mod releases {
    pub use crate::screen::releases::{ReleaseViewData, ReleasesState};
}

pub mod spark_detail {
    pub use crate::screen::spark_detail::{
        AcceptanceCriteriaEdit, AssigneeEditState, ContractCreateForm, Field, ProblemEditState,
        SparkEdit, SparkEditSession,
    };
}

pub mod sparks {
    pub use crate::screen::sparks::{
        CreateForm, SortMode, SparksFilter, StatusMenu, spark_type_rank, status_rank,
    };
}
