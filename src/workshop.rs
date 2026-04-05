// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! A Workshop is a self-contained workspace bound to a directory.
//! Each workshop has its own `.forge/` directory containing config,
//! sparks database, agent definitions, and context files.

use std::collections::HashMap;
use std::path::PathBuf;

use data::forge_dir::{AgentDef, ForgeDir, WorkshopConfig};
use data::sparks::types::Spark;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::coding_agents::CodingAgent;
use crate::screen::agents::AgentSession;
use crate::screen::bench::{BenchState, TabKind};

pub struct Workshop {
    pub id: Uuid,
    pub directory: PathBuf,
    pub forge_dir: ForgeDir,
    pub config: WorkshopConfig,
    pub bench: BenchState,
    pub terminals: HashMap<u64, iced_term::Terminal>,
    pub agent_sessions: Vec<AgentSession>,
    /// Sparks database for this workshop.
    pub sparks_db: Option<SqlitePool>,
    /// Cached sparks for display (loaded from DB).
    pub sparks: Vec<Spark>,
    /// Custom agent definitions from `.forge/agents/`.
    pub custom_agents: Vec<AgentDef>,
    /// Agent context from `.forge/context/AGENTS.md`.
    pub agent_context: Option<String>,
}

impl Workshop {
    pub fn new(directory: PathBuf) -> Self {
        let forge_dir = ForgeDir::new(&directory);
        Self {
            id: Uuid::new_v4(),
            directory,
            forge_dir,
            config: WorkshopConfig::default(),
            bench: BenchState::new(),
            terminals: HashMap::new(),
            agent_sessions: Vec::new(),
            sparks_db: None,
            sparks: Vec::new(),
            custom_agents: Vec::new(),
            agent_context: None,
        }
    }

    /// Display name — from config, or last path component.
    pub fn name(&self) -> &str {
        self.config
            .name
            .as_deref()
            .unwrap_or_else(|| {
                self.directory
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("workshop")
            })
    }

    /// Sidebar split ratio from config.
    pub fn sidebar_split(&self) -> f32 {
        self.config.layout.sidebar_split
    }

    /// Sidebar width from config.
    pub fn sidebar_width(&self) -> f32 {
        self.config.layout.sidebar_width
    }

    /// Sparks panel width from config.
    pub fn sparks_width(&self) -> f32 {
        self.config.layout.sparks_width
    }

    /// Spawn a terminal tab, optionally running a coding agent command.
    pub fn spawn_terminal(
        &mut self,
        title: String,
        agent: Option<&CodingAgent>,
        next_terminal_id: &mut u64,
    ) -> u64 {
        let kind = match agent {
            Some(a) => TabKind::CodingAgent(a.clone()),
            None => TabKind::Terminal,
        };

        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;
        self.bench.create_tab(tab_id, title, kind);

        let mut settings = iced_term::settings::Settings::default();
        settings.font.size = 14.0;
        settings.backend.working_directory = Some(self.directory.clone());

        if let Some(agent) = agent {
            settings.backend.program = agent.command.clone();
            settings.backend.args = agent.args.clone();
        } else {
            let shell = std::env::var("SHELL")
                .unwrap_or_else(|_| "/bin/bash".to_string());
            settings.backend.program = shell;
        }

        if let Ok(term) = iced_term::Terminal::new(tab_id, settings) {
            self.terminals.insert(tab_id, term);
        }

        tab_id
    }

    /// Spawn a terminal for a custom agent definition.
    pub fn spawn_custom_agent(
        &mut self,
        def: &AgentDef,
        next_terminal_id: &mut u64,
    ) -> u64 {
        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;
        self.bench.create_tab(
            tab_id,
            def.name.clone(),
            TabKind::CodingAgent(CodingAgent {
                display_name: def.name.clone(),
                command: def.command.clone(),
                args: def.args.clone(),
            }),
        );

        let mut settings = iced_term::settings::Settings::default();
        settings.font.size = 14.0;
        settings.backend.working_directory = Some(self.directory.clone());
        settings.backend.program = def.command.clone();
        settings.backend.args = def.args.clone();
        for (k, v) in &def.env {
            settings.backend.env.insert(k.clone(), v.clone());
        }

        if let Ok(term) = iced_term::Terminal::new(tab_id, settings) {
            self.terminals.insert(tab_id, term);
        }

        tab_id
    }

    /// Handle terminal shutdown/title-change for a given terminal id.
    pub fn handle_terminal_action(
        &mut self,
        id: u64,
        action: iced_term::actions::Action,
    ) {
        match action {
            iced_term::actions::Action::Shutdown => {
                self.terminals.remove(&id);
                self.agent_sessions.retain(|s| s.tab_id != id);
                self.bench.close_tab(id);
            }
            iced_term::actions::Action::ChangeTitle(title) => {
                if let Some(tab) =
                    self.bench.tabs.iter_mut().find(|t| t.id == id)
                {
                    tab.title = title;
                }
            }
            iced_term::actions::Action::Ignore => {}
        }
    }
}

/// Result of async workshop initialization.
pub struct WorkshopInit {
    pub pool: SqlitePool,
    pub config: WorkshopConfig,
    pub custom_agents: Vec<AgentDef>,
    pub agent_context: Option<String>,
}

/// Initialize a workshop's `.forge/` directory, DB, and load config.
/// This is the single async entry point called when a workshop opens.
pub async fn init_workshop(
    directory: PathBuf,
) -> Result<WorkshopInit, data::sparks::SparksError> {
    let forge_dir = ForgeDir::new(&directory);

    // Create directory structure + default files
    data::forge_dir::init_forge_dir(&forge_dir)
        .await
        .map_err(data::sparks::SparksError::Io)?;

    // Open/migrate database
    let pool = data::db::open_sparks_db(&directory).await?;

    // Load config and agents in parallel
    let config = data::forge_dir::load_config(&forge_dir).await;
    let custom_agents = data::forge_dir::load_agent_defs(&forge_dir).await;
    let agent_context = data::forge_dir::load_agents_context(&forge_dir).await;

    Ok(WorkshopInit {
        pool,
        config,
        custom_agents,
        agent_context,
    })
}
