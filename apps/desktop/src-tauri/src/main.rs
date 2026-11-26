#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod glassdeckd;
mod library;

use glassdeck_api::{Status, Theme};
use glassdeckd::{Glassdeckd, GlassdeckdError, GlassdeckdUpdate};
use library::{LibraryError, NewWidget, WidgetSummary};
use specta_typescript::Typescript;
use std::path::Path;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_specta::{Builder, ErrorHandlingMode, collect_commands, collect_events};

const BINDINGS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/bindings.ts");

#[tauri::command]
#[specta::specta]
async fn status(glassdeckd: State<'_, Glassdeckd>) -> Result<Status, GlassdeckdError> {
    glassdeckd.status().await
}

#[tauri::command]
#[specta::specta]
async fn theme(glassdeckd: State<'_, Glassdeckd>) -> Result<Theme, GlassdeckdError> {
    glassdeckd.theme().await
}

/// Runs a library call off the async runtime, with the library's folder.
async fn in_library<T: Send + 'static>(call: impl FnOnce(&Path) -> Result<T, LibraryError> + Send + 'static) -> Result<T, LibraryError> {
    tauri::async_runtime::spawn_blocking(move || call(&library::root()?))
        .await
        .map_err(|e| LibraryError::Io { reason: e.to_string() })?
}

#[tauri::command]
#[specta::specta]
async fn list_widgets() -> Result<Vec<WidgetSummary>, LibraryError> {
    in_library(library::list_widgets).await
}

#[tauri::command]
#[specta::specta]
async fn create_widget(widget: NewWidget) -> Result<WidgetSummary, LibraryError> {
    in_library(move |root| library::create_widget(root, widget)).await
}

#[tauri::command]
#[specta::specta]
async fn duplicate_widget(id: String) -> Result<WidgetSummary, LibraryError> {
    in_library(move |root| library::duplicate_widget(root, &id)).await
}

#[tauri::command]
#[specta::specta]
async fn rename_widget(id: String, title: String) -> Result<WidgetSummary, LibraryError> {
    in_library(move |root| library::rename_widget(root, &id, &title)).await
}

#[tauri::command]
#[specta::specta]
async fn delete_widget(id: String) -> Result<(), LibraryError> {
    in_library(move |root| library::delete_widget(root, &id)).await
}

#[tauri::command]
#[specta::specta]
async fn reveal_widget(id: String) -> Result<(), LibraryError> {
    in_library(move |root| {
        let folder = library::widget_folder(root, &id)?;
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", folder.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| LibraryError::Io { reason: format!("opening Explorer: {e}") })
    })
    .await
}

/// Asks where to save, then packs the widget there. `None` when the dialog is cancelled.
#[tauri::command]
#[specta::specta]
async fn export_widget(app: AppHandle, id: String) -> Result<Option<String>, LibraryError> {
    in_library(move |root| {
        let picked = app
            .dialog()
            .file()
            .add_filter("Glassdeck package", &[glassdeck_package::EXTENSION])
            .set_file_name(format!("{id}.{}", glassdeck_package::EXTENSION))
            .blocking_save_file();
        let Some(out) = picked.and_then(|p| p.into_path().ok()) else {
            return Ok(None);
        };
        library::export_widget(root, &id, &out)?;
        Ok(Some(out.display().to_string()))
    })
    .await
}

/// Asks for a `.deck`, then unpacks it into the library. `None` when the dialog is cancelled.
#[tauri::command]
#[specta::specta]
async fn import_widget(app: AppHandle) -> Result<Option<WidgetSummary>, LibraryError> {
    in_library(move |root| {
        let picked = app.dialog().file().add_filter("Glassdeck package", &[glassdeck_package::EXTENSION]).blocking_pick_file();
        let Some(deck) = picked.and_then(|p| p.into_path().ok()) else {
            return Ok(None);
        };
        library::import_widget(root, &deck).map(Some)
    })
    .await
}

fn bindings() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            status,
            theme,
            list_widgets,
            create_widget,
            duplicate_widget,
            rename_widget,
            delete_widget,
            reveal_widget,
            export_widget,
            import_widget,
        ])
        .events(collect_events![GlassdeckdUpdate])
        .constant("WIDGET_AREA", library::widget_area())
        .constant("GRID", library::GRID)
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
        .plugin(tauri_plugin_dialog::init())
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
