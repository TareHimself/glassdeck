#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod glassdeckd;
mod library;
mod options;
mod preview;

use glassdeck_api::{Status, Theme};
use glassdeckd::{Glassdeckd, GlassdeckdError, GlassdeckdUpdate};
use library::{Dims, LibraryError, NewWidget, SourceFile, WidgetEdit, WidgetSource, WidgetSummary};
use options::OptionsPanel;
use preview::{Preview, PreviewUpdate};
use specta_typescript::Typescript;
use std::path::{Path, PathBuf};

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

#[tauri::command]
#[specta::specta]
async fn read_widget(id: String) -> Result<WidgetSource, LibraryError> {
    in_library(move |root| library::read_widget(root, &id)).await
}

#[tauri::command]
#[specta::specta]
async fn save_widget(id: String, edit: WidgetEdit) -> Result<WidgetSummary, LibraryError> {
    in_library(move |root| library::save_widget(root, &id, edit)).await
}

/// Where the native libraries are: `GLASSDECK_NATIVE`, else beside this program, else (a dev build in
/// `target/debug`) `target/native`.
fn native_libraries() -> Result<PathBuf, LibraryError> {
    if let Some(dir) = std::env::var_os("GLASSDECK_NATIVE") {
        return Ok(PathBuf::from(dir));
    }
    let unavailable = |reason: String| LibraryError::CheckerUnavailable { reason };
    let exe = std::env::current_exe().map_err(|e| unavailable(format!("finding this program: {e}")))?;
    let beside = exe.parent().ok_or_else(|| unavailable("this program has no folder".into()))?.to_path_buf();
    let dev = beside.join("..").join("native");
    Ok(if dev.is_dir() { dev } else { beside })
}

/// Type-checks the widget's `options.luau` and `host.luau` as saved; each problem is "name:line:column: message".
#[tauri::command]
#[specta::specta]
async fn check_widget(id: String) -> Result<Vec<String>, LibraryError> {
    in_library(move |root| {
        let folder = library::widget_folder(root, &id)?;
        let unavailable = |reason: String| LibraryError::CheckerUnavailable { reason };
        let luau = glassdeck_luau_check::LuauChecker::load(&native_libraries()?).map_err(|e| unavailable(format!("{e:#}")))?;
        glassdeck_slang::pack::widget_problems(&luau, &folder).map_err(|e| unavailable(format!("{e:#}")))
    })
    .await
}

#[tauri::command]
#[specta::specta]
fn widget_options(source: String, values: serde_json::Map<String, serde_json::Value>) -> OptionsPanel {
    options::options_panel(&source, &values)
}

/// Starts the preview of a widget with the editor's unsaved scripts; frames arrive as `PreviewUpdate`s.
#[tauri::command]
#[specta::specta]
async fn preview_widget(
    preview: State<'_, Preview>,
    id: String,
    files: Vec<SourceFile>,
    size: Dims,
    values: serde_json::Map<String, serde_json::Value>,
) -> Result<(), LibraryError> {
    let folder = in_library(move |root| library::widget_folder(root, &id)).await?;
    preview.send(preview::Command::Show { folder, files, size, values });
    Ok(())
}

#[tauri::command]
#[specta::specta]
fn preview_resize(preview: State<'_, Preview>, size: Dims) {
    preview.send(preview::Command::Resize(size));
}

#[tauri::command]
#[specta::specta]
fn preview_options(preview: State<'_, Preview>, values: serde_json::Map<String, serde_json::Value>) {
    preview.send(preview::Command::Options(values));
}

#[tauri::command]
#[specta::specta]
fn stop_preview(preview: State<'_, Preview>) {
    preview.send(preview::Command::Stop);
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
            read_widget,
            save_widget,
            check_widget,
            widget_options,
            preview_widget,
            preview_resize,
            preview_options,
            stop_preview,
        ])
        .events(collect_events![GlassdeckdUpdate, PreviewUpdate])
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
            app.manage(Preview::start(app.handle().clone()));
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
