fn main() {
    let commands = tauri_build::AppManifest::new().commands(&["status"]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(commands)).expect("running tauri-build");
}
