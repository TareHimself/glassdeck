#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod glassdeckd;

use glassdeck_api::Status;
use glassdeckd::{Glassdeckd, GlassdeckdError, GlassdeckdUpdate};
use specta_typescript::Typescript;
use tauri::{Manager, State};
use tauri_specta::{Builder, ErrorHandlingMode, collect_commands, collect_events};

const BINDINGS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/bindings.ts");

#[tauri::command]
#[specta::specta]
async fn status(glassdeckd: State<'_, Glassdeckd>) -> Result<Status, GlassdeckdError> {
    glassdeckd.status().await
}

fn bindings() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![status])
        .events(collect_events![GlassdeckdUpdate])
        .error_handling(ErrorHandlingMode::Result)
        // Option values are serde_json::Value, whose numbers JavaScript parses as doubles anyway.
        .dangerously_cast_bigints_to_number()
        .disable_serde_phases()
}

fn main() {
    let bindings = bindings();
    #[cfg(debug_assertions)]
    bindings.export(Typescript::default(), BINDINGS).expect("exporting TypeScript bindings");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let Some(window) = app.get_webview_window("main") else {
                eprintln!("a second launch found no main window to focus");
                return;
            };
            if let Err(e) = window.unminimize().and_then(|()| window.set_focus()) {
                eprintln!("focusing the main window for a second launch: {e}");
            }
        }))
        .manage(Glassdeckd::new())
        .invoke_handler(bindings.invoke_handler())
        .setup(move |app| {
            bindings.mount_events(app);
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                handle.state::<Glassdeckd>().relay_events(handle.clone()).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("running the Glassdeck app");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_bindings_match_the_rust_types() {
        let fresh = std::env::temp_dir().join("glassdeck-desktop-bindings.ts");
        bindings().export(Typescript::default(), &fresh).expect("exporting TypeScript bindings");
        let committed = std::fs::read_to_string(BINDINGS).unwrap_or_default();
        let fresh = std::fs::read_to_string(&fresh).expect("reading the fresh bindings");
        assert!(committed == fresh, "src/bindings.ts is stale: run the app in debug once (pnpm tauri dev) and commit it");
    }
}
