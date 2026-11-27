#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backgrounds;
mod canvas_preview;
mod videos;
mod glassdeckd;
mod layout_preview;
mod library;
mod lsp;
mod options;
mod preview;

use backgrounds::{BackgroundSummary, CanvasCheck, CanvasSource, NewBackground};
use canvas_preview::{CanvasPreview, CanvasUpdate};
use layout_preview::{LayoutPreview, LayoutUpdate, PlacedSource};
use videos::{Conversions, VideoBackground, VideoCut, VideoFacts, VideoUpdate};
use glassdeck_api::{Status, Streams, Theme, ThemeApplied, ThemePush};
use glassdeck_package::PackageType;
use glassdeckd::{Glassdeckd, GlassdeckdError, GlassdeckdUpdate};
use library::{Dims, LibraryError, NewWidget, SourceFile, SourceEdit, WidgetSource, WidgetSummary};
use options::OptionsPanel;
use preview::{Preview, PreviewUpdate};
use serde::Serialize;
use specta::Type;
use specta_typescript::Typescript;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_specta::{Builder, ErrorHandlingMode, Event as _, collect_commands, collect_events};

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

#[tauri::command]
#[specta::specta]
async fn streams(glassdeckd: State<'_, Glassdeckd>) -> Result<Streams, GlassdeckdError> {
    glassdeckd.streams().await
}

/// Why Apply didn't reach the keyboard.
#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ApplyError {
    /// A package couldn't be packed from the library
    Library { error: LibraryError },
    Glassdeckd { error: GlassdeckdError },
}

/// Where Apply leaves each package for glassdeckd to read, overwritten on every Apply.
fn outbox() -> Result<PathBuf, LibraryError> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or(LibraryError::Io { reason: "LOCALAPPDATA isn't set".into() })?;
    let folder = PathBuf::from(local).join("Glassdeck").join("outbox");
    std::fs::create_dir_all(&folder).map_err(|e| LibraryError::Io { reason: format!("creating {}: {e}", folder.display()) })?;
    Ok(folder)
}

/// Packs every package `theme` uses from the library, the built-in background aside, then makes it the active theme.
#[tauri::command]
#[specta::specta]
async fn apply_theme(glassdeckd: State<'_, Glassdeckd>, theme: Theme) -> Result<ThemeApplied, ApplyError> {
    let used = std::iter::once(theme.background.package.clone())
        .filter(|id| id != glassdeck_proto::BUILT_IN_BACKGROUND)
        .chain(theme.widgets.iter().map(|w| w.package.clone()))
        .collect::<std::collections::BTreeSet<String>>();
    let packages = in_library(move |root| {
        let outbox = outbox()?;
        let native = native_libraries()?;
        let mut paths = std::collections::BTreeMap::new();
        for id in used {
            let (qualified, _) = library::package_folder(root, &id)?;
            let out = outbox.join(qualified.file_name());
            match qualified.package_type() {
                PackageType::Widget => library::export_widget(root, &id, &out)?,
                PackageType::Background => backgrounds::export_background(root, &id, &out, &native)?,
            }
            paths.insert(id, out.display().to_string());
        }
        Ok(paths)
    })
    .await
    .map_err(|error| ApplyError::Library { error })?;
    glassdeckd.put_theme(&ThemePush { theme, packages }).await.map_err(|error| ApplyError::Glassdeckd { error })
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
async fn list_backgrounds() -> Result<Vec<BackgroundSummary>, LibraryError> {
    in_library(backgrounds::list_backgrounds).await
}

#[tauri::command]
#[specta::specta]
async fn create_background(app: AppHandle, conversions: State<'_, Conversions>, background: NewBackground) -> Result<BackgroundSummary, LibraryError> {
    let (made, conversion) = in_library(move |root| backgrounds::create_background(root, background)).await?;
    if let Some(conversion) = conversion {
        conversions.start(app, conversion);
    }
    Ok(made)
}

/// Runs every widget of the Overview's layout; their tiles arrive as `LayoutUpdate`s.
#[tauri::command]
#[specta::specta]
async fn preview_layout(preview: State<'_, LayoutPreview>, widgets: Vec<PlacedSource>, run: preview::PreviewRun) -> Result<(), LibraryError> {
    let layout = in_library(move |root| {
        Ok(widgets.into_iter().map(|w| (library::widget_folder(root, &w.package).ok(), w)).collect::<Vec<_>>())
    })
    .await?;
    preview.send(layout_preview::Command::Show(run, layout));
    Ok(())
}

#[tauri::command]
#[specta::specta]
fn stop_layout(preview: State<'_, LayoutPreview>) {
    preview.send(layout_preview::Command::Stop);
}

#[tauri::command]
#[specta::specta]
fn canvas_template() -> Vec<SourceFile> {
    backgrounds::canvas_template()
}

/// Asks Media Foundation what a video file the user chose is, and allows it for playback in the app.
#[tauri::command]
#[specta::specta]
async fn video_facts(app: AppHandle, path: String) -> Result<VideoFacts, LibraryError> {
    let file = PathBuf::from(&path);
    let facts = in_library(move |_| videos::facts(&file)).await?;
    app.asset_protocol_scope().allow_file(&path).map_err(|e| LibraryError::Io { reason: format!("allowing {path} for playback: {e}") })?;
    Ok(facts)
}

/// The frame at `seconds` of a video file, as a base64 JPEG.
#[tauri::command]
#[specta::specta]
async fn video_frame(path: String, seconds: f64) -> Result<String, LibraryError> {
    in_library(move |_| videos::source_frame(Path::new(&path), seconds)).await
}

/// A video background, with its converted file allowed for playback in the app.
#[tauri::command]
#[specta::specta]
async fn read_video(app: AppHandle, conversions: State<'_, Conversions>, id: String) -> Result<VideoBackground, LibraryError> {
    let conversions = conversions.inner().clone();
    let video = in_library(move |root| videos::read_video(root, &id, &conversions)).await?;
    if let Some(converted) = &video.video {
        app.asset_protocol_scope()
            .allow_file(&converted.path)
            .map_err(|e| LibraryError::Io { reason: format!("allowing {} for playback: {e}", converted.path) })?;
    }
    Ok(video)
}

/// Sets the preview image to the converted video's frame at `seconds`; the new image, as a base64 JPEG.
#[tauri::command]
#[specta::specta]
async fn set_video_preview(id: String, seconds: f64) -> Result<String, LibraryError> {
    in_library(move |root| videos::set_preview(root, &id, seconds)).await
}

/// Converts a new file into the background, cut as given.
#[tauri::command]
#[specta::specta]
async fn replace_video(app: AppHandle, conversions: State<'_, Conversions>, id: String, source: String, cut: VideoCut) -> Result<(), LibraryError> {
    let conversion = in_library(move |root| videos::replace(root, &id, Path::new(&source), cut)).await?;
    conversions.start(app, conversion);
    Ok(())
}

#[tauri::command]
#[specta::specta]
async fn read_canvas(id: String) -> Result<CanvasSource, LibraryError> {
    in_library(move |root| backgrounds::read_canvas(root, &id)).await
}

#[tauri::command]
#[specta::specta]
async fn save_canvas(id: String, edit: SourceEdit) -> Result<BackgroundSummary, LibraryError> {
    in_library(move |root| backgrounds::save_canvas(root, &id, edit)).await
}

/// Compiles the canvas as saved and type-checks its scripts.
#[tauri::command]
#[specta::specta]
async fn check_canvas(id: String) -> Result<CanvasCheck, LibraryError> {
    in_library(move |root| backgrounds::check_canvas(root, &id, &native_libraries()?)).await
}

/// Compiles the editor's unsaved files and starts drawing them; frames arrive as `CanvasUpdate`s.
#[tauri::command]
#[specta::specta]
fn preview_canvas(preview: State<'_, CanvasPreview>, files: Vec<SourceFile>, values: serde_json::Map<String, serde_json::Value>, run: preview::PreviewRun) {
    preview.send(canvas_preview::Command::Show { run, files, values });
}

#[tauri::command]
#[specta::specta]
fn canvas_options(preview: State<'_, CanvasPreview>, values: serde_json::Map<String, serde_json::Value>) {
    preview.send(canvas_preview::Command::Options(values));
}

#[tauri::command]
#[specta::specta]
fn play_canvas(preview: State<'_, CanvasPreview>, playing: bool) {
    preview.send(canvas_preview::Command::Play(playing));
}

#[tauri::command]
#[specta::specta]
fn restart_canvas(preview: State<'_, CanvasPreview>) {
    preview.send(canvas_preview::Command::Restart);
}

#[tauri::command]
#[specta::specta]
fn stop_canvas(preview: State<'_, CanvasPreview>) {
    preview.send(canvas_preview::Command::Stop);
}

/// A rectangle on the 1920x1080 panel, in panel pixels.
#[derive(Clone, Copy, Debug, Serialize, Type)]
struct PanelRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Clone, Debug, Serialize, Type)]
struct PanelLayout {
    keys: Vec<PanelRect>,
    widget_area: PanelRect,
}

/// Where the keys and the widget area sit on the panel, for the canvas preview's overlays.
#[tauri::command]
#[specta::specta]
fn panel_layout() -> PanelLayout {
    let layout = glassdeck_layout::Layout::new(glassdeck_layout::Variant::Ansi);
    let rect = |r: &glassdeck_layout::Rect| PanelRect { x: r.x, y: r.y, w: r.width, h: r.height };
    PanelLayout { keys: layout.keys.iter().map(rect).collect(), widget_area: rect(&layout.widget_area) }
}

/// Starts the language server an editor tab needs, working in the package's library folder.
#[tauri::command]
#[specta::specta]
async fn lsp_start(sessions: State<'_, lsp::Sessions>, server: lsp::LspServer, id: String) -> Result<lsp::LspStarted, lsp::LspError> {
    let (id, folder) = in_library(move |root| library::package_folder(root, &id)).await.map_err(|error| lsp::LspError::Library { error })?;
    let native = native_libraries().map_err(|error| lsp::LspError::Library { error })?;
    let session = sessions.start(&lsp::launch(server, id.package_type(), &folder, &native)?)?;
    Ok(lsp::LspStarted { session, root: lsp::file_uri(&folder), settings: lsp::settings(server, &folder) })
}

/// Sends one JSON-RPC message to a running language server.
#[tauri::command]
#[specta::specta]
async fn lsp_send(sessions: State<'_, lsp::Sessions>, session: lsp::LspSession, message: String) -> Result<(), lsp::LspError> {
    sessions.send(session, &message)
}

#[tauri::command]
#[specta::specta]
async fn lsp_stop(sessions: State<'_, lsp::Sessions>, session: lsp::LspSession) -> Result<(), lsp::LspError> {
    sessions.stop(session)
}

#[tauri::command]
#[specta::specta]
async fn create_widget(widget: NewWidget) -> Result<WidgetSummary, LibraryError> {
    in_library(move |root| library::create_widget(root, widget)).await
}

/// Copies a widget or background; the copy's qualified id.
#[tauri::command]
#[specta::specta]
async fn duplicate_package(id: String) -> Result<String, LibraryError> {
    in_library(move |root| library::duplicate_package(root, &id).map(|copy| copy.to_string())).await
}

#[tauri::command]
#[specta::specta]
async fn rename_package(id: String, title: String) -> Result<(), LibraryError> {
    in_library(move |root| library::rename_package(root, &id, &title)).await
}

#[tauri::command]
#[specta::specta]
async fn delete_package(id: String) -> Result<(), LibraryError> {
    in_library(move |root| library::delete_package(root, &id)).await
}

#[tauri::command]
#[specta::specta]
async fn reveal_package(id: String) -> Result<(), LibraryError> {
    in_library(move |root| {
        let (_, folder) = library::package_folder(root, &id)?;
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
async fn save_widget(id: String, edit: SourceEdit) -> Result<WidgetSummary, LibraryError> {
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

/// What checking a widget found, each problem as "name:line:column: message", and how many scripts it checked.
#[derive(Clone, Debug, Serialize, Type)]
struct WidgetCheck {
    problems: Vec<String>,
    checked: u32,
}

/// Type-checks the widget's `options.luau` and `host.luau` as saved, skipping blank ones.
#[tauri::command]
#[specta::specta]
async fn check_widget(id: String) -> Result<WidgetCheck, LibraryError> {
    in_library(move |root| {
        let folder = library::widget_folder(root, &id)?;
        let unavailable = |reason: String| LibraryError::CheckerUnavailable { reason };
        let luau = glassdeck_luau_check::LuauChecker::load(&native_libraries()?).map_err(|e| unavailable(format!("{e:#}")))?;
        let check = glassdeck_slang::pack::widget_problems(&luau, &folder).map_err(|e| unavailable(format!("{e:#}")))?;
        Ok(WidgetCheck { problems: check.problems, checked: check.checked.len() as u32 })
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
    run: preview::PreviewRun,
) -> Result<(), LibraryError> {
    let folder = in_library(move |root| library::widget_folder(root, &id)).await?;
    preview.send(preview::Command::Show { run, folder, files, size, values });
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

/// Asks where to save, then packs the widget or background there. `None` when the dialog is cancelled.
#[tauri::command]
#[specta::specta]
async fn export_package(app: AppHandle, id: String) -> Result<Option<String>, LibraryError> {
    in_library(move |root| {
        let (qualified, _) = library::package_folder(root, &id)?;
        let picked = app
            .dialog()
            .file()
            .add_filter("Glassdeck package", &[glassdeck_package::EXTENSION])
            .set_file_name(qualified.file_name())
            .blocking_save_file();
        let Some(out) = picked.and_then(|p| p.into_path().ok()) else {
            return Ok(None);
        };
        match qualified.package_type() {
            PackageType::Widget => library::export_widget(root, &id, &out)?,
            PackageType::Background => backgrounds::export_background(root, &id, &out, &native_libraries()?)?,
        }
        Ok(Some(out.display().to_string()))
    })
    .await
}

/// Asks for a video file to make a background from. `None` when the dialog is cancelled.
#[tauri::command]
#[specta::specta]
async fn pick_video(app: AppHandle) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let picked = app.dialog().file().add_filter("Video", &VIDEO_EXTENSIONS).blocking_pick_file();
        picked.and_then(|p| p.into_path().ok()).map(|p| p.display().to_string())
    })
    .await
    .ok()
    .flatten()
}

/// What Media Foundation reads on Windows without extra codecs.
const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "m4v", "mov", "mkv", "webm", "wmv"];

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
            streams,
            apply_theme,
            list_widgets,
            create_widget,
            duplicate_package,
            rename_package,
            delete_package,
            reveal_package,
            list_backgrounds,
            create_background,
            canvas_template,
            preview_layout,
            stop_layout,
            video_facts,
            video_frame,
            read_video,
            set_video_preview,
            replace_video,
            read_canvas,
            save_canvas,
            check_canvas,
            preview_canvas,
            canvas_options,
            play_canvas,
            restart_canvas,
            stop_canvas,
            panel_layout,
            export_package,
            pick_video,
            import_widget,
            read_widget,
            save_widget,
            check_widget,
            widget_options,
            preview_widget,
            preview_resize,
            preview_options,
            stop_preview,
            lsp_start,
            lsp_send,
            lsp_stop,
        ])
        .events(collect_events![GlassdeckdUpdate, PreviewUpdate, CanvasUpdate, VideoUpdate, LayoutUpdate, lsp::LspMessage, lsp::LspExited])
        .constant("WIDGET_AREA", library::widget_area())
        .constant("GRID", library::GRID)
        .constant("BUILT_IN_BACKGROUND", glassdeck_proto::BUILT_IN_BACKGROUND)
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
            app.manage(Conversions::default());
            app.manage(LayoutPreview::start(app.handle().clone()));
            app.manage(CanvasPreview::start(app.handle().clone(), native_libraries().ok()));
            let lsp_events = app.handle().clone();
            app.manage(lsp::Sessions::new(move |output| {
                let emitted = match output {
                    lsp::Output::Message(message) => message.emit(&lsp_events),
                    lsp::Output::Exited(exited) => exited.emit(&lsp_events),
                };
                if let Err(e) = emitted {
                    eprintln!("emitting a language server event: {e}");
                }
            }));
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
