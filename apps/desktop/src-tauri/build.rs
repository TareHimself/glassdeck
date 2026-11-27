fn main() {
    let commands = tauri_build::AppManifest::new().commands(&["status", "theme", "list_widgets", "create_widget", "duplicate_package", "rename_package", "delete_package", "reveal_package", "export_package", "pick_video", "import_widget", "read_widget", "save_widget", "check_widget", "widget_options", "preview_widget", "preview_resize", "preview_options", "stop_preview", "list_backgrounds", "create_background", "read_canvas", "save_canvas", "check_canvas", "preview_canvas", "canvas_options", "play_canvas", "restart_canvas", "stop_canvas", "panel_layout"]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(commands)).expect("running tauri-build");
}
