fn main() {
    let commands = tauri_build::AppManifest::new().commands(&["status", "theme", "list_widgets", "create_widget", "duplicate_widget", "rename_widget", "delete_widget", "reveal_widget", "export_widget", "import_widget"]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(commands)).expect("running tauri-build");
}
