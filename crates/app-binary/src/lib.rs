// Rust 1.98 surfaces the localized MSVC link.exe progress message as a
// `linker_messages` warning even though the link succeeds.
#![allow(linker_messages)]

mod agent_tool_adapters;
mod app_state;
mod autostart;
mod bootstrap;
mod chatgpt_plan_auth;
mod chatgpt_plan_login;
mod commands;
mod config_runtime;
mod desktop;
mod event_bridge;
mod events;
mod failed_transcriptions;
mod handlers;
mod logging;
mod notification;
mod recording_lifecycle;
mod router_media_builder;
mod runtime;
mod startup_error;
mod ui_assets;
#[cfg(windows)]
mod window_resize;

pub(crate) use bootstrap::to_tauri_shortcut;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    bootstrap::run();
}

#[cfg(test)]
mod tests;
