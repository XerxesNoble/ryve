// SPDX-License-Identifier: AGPL-3.0-or-later

mod agent_prompts;
mod app;
mod bundled_ircd;
mod bundled_tmux;
mod cli;
mod coding_agents;
mod delegation;
mod font_intern;
mod hand_archetypes;
mod hand_spawn;
mod head;
mod head_archetype;
mod head_archetypes;
mod icons;
mod ircd_process;
mod panel_state;
mod process_snapshot;
mod release_artifact;
mod screen;
#[allow(dead_code)]
mod sparks_filter;
// Some wrapper API surface (builder getters, custom-heartbeat-line
// setter) is intentionally kept for future callers / integration tests
// under `tests/heavy_test_stream_survival.rs` (spark ryve-9350365a),
// even though the binary entry-point only uses a subset today.
#[allow(dead_code)]
mod stream_heartbeat;
mod style;
mod tmux;
mod watch_runner;
mod widget;
mod workshop;
mod worktree_cleanup;

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().collect();
    let first_non_flag = args
        .iter()
        .skip(1)
        .find(|a| a.as_str() != "--json")
        .map(|s| s.as_str());

    if let Some(cmd) = first_non_flag
        && cli::CLI_COMMANDS.contains(&cmd)
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        rt.block_on(cli::run(args));
        return Ok(());
    }

    let invocation = ipc::ForwardedInvocation::from_env();
    match ipc::acquire(&invocation) {
        Ok(ipc::Acquired::Forwarded) => {
            return Ok(());
        }
        #[cfg(unix)]
        Ok(ipc::Acquired::First { listener }) => {
            app::store_ipc_listener(listener);
        }
        #[cfg(not(unix))]
        Ok(ipc::Acquired::First {}) => {}
        Err(e) => {
            eprintln!(
                "ryve: single-instance check failed ({e}); starting anyway. \
                 Multiple ryve windows may run simultaneously."
            );
        }
    }

    let config = data::config::Config::load();
    let default_font = match config.font_family {
        Some(name) => iced::Font {
            family: iced::font::Family::Name(font_intern::intern(&name)),
            ..iced::Font::DEFAULT
        },
        None => iced::Font {
            family: iced::font::Family::SansSerif,
            ..iced::Font::DEFAULT
        },
    };

    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut window = iced::window::Settings {
        size: iced::Size::new(1400.0, 900.0),
        min_size: Some(iced::Size::new(480.0, 400.0)),
        transparent: true,
        ..Default::default()
    };

    #[cfg(target_os = "macos")]
    {
        window.platform_specific.title_hidden = true;
        window.platform_specific.titlebar_transparent = true;
        window.platform_specific.fullsize_content_view = true;
    }

    iced::application(app::App::boot, app::App::update, app::App::view)
        .title("Ryve")
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .default_font(default_font)
        .window(window)
        .run()
}
