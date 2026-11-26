fn main() {
    let commands = tauri_build::AppManifest::new().commands(&["status", "theme", "list_widgets", "create_widget", "duplicate_widget", "rename_widget", "delete_widget", "reveal_widget", "export_widget", "import_widget", "read_widget", "save_widget", "check_widget", "widget_options", "preview_widget", "preview_resize", "preview_options", "stop_preview"]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(commands)).expect("running tauri-build");
}
