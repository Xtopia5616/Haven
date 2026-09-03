fn main() {
    tauri_build::build();

    // `tauri::generate_context!` embeds the Windows default icon at compile
    // time, but tauri-build does not register bundle icon files as Cargo
    // inputs. Without these declarations, `cargo tauri dev` can keep using an
    // older embedded icon after the ICO/PNG assets have been regenerated.
    for icon in [
        "icons/icon.ico",
        "icons/icon.png",
        "icons/32x32.png",
        "icons/64x64.png",
        "icons/128x128.png",
        "icons/128x128@2x.png",
    ] {
        println!("cargo:rerun-if-changed={icon}");
    }
}
