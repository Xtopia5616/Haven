// Rust 1.98 surfaces the localized MSVC link.exe progress message as a
// `linker_messages` warning even though the link succeeds.
#![allow(linker_messages)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tokio::main]
async fn main() {
    haven_app_binary_lib::run();
}
