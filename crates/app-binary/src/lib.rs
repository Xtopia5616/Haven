// Rust 1.98 surfaces the localized MSVC link.exe progress message as a
// `linker_messages` warning even though the link succeeds.
#![allow(linker_messages)]

mod app_state;
mod autostart;
mod bootstrap;
mod commands;
mod config_runtime;
mod desktop;
mod event_bridge;
mod events;
mod handlers;
mod logging;
mod notification;

pub(crate) use bootstrap::to_tauri_shortcut;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    bootstrap::run();
}

#[cfg(test)]
mod tests;
