//! The tray icon: closing the window hides the app there instead of quitting, and its previews stop
//! drawing until it's shown again. The keyboard never notices; glassdeckd runs it either way.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::{canvas_preview, layout_preview, preview};

const OPEN: &str = "open";
const QUIT: &str = "quit";

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let menu = Menu::with_items(app, &[&MenuItem::with_id(app, OPEN, "Open Glassdeck", true, None::<&str>)?, &MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip("Glassdeck")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            OPEN => show(app),
            QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Brings the window back, from the tray or a second launch, and lets the previews draw again.
pub fn show(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return eprintln!("there's no main window to show");
    };
    if let Err(e) = window.show().and_then(|()| window.unminimize()).and_then(|()| window.set_focus()) {
        eprintln!("showing the window: {e}");
    }
    set_previews_visible(app, true);
}

/// Hides the window to the tray; the previews stop drawing.
pub fn hide(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else { return };
    if let Err(e) = window.hide() {
        eprintln!("hiding the window: {e}");
    }
    set_previews_visible(app, false);
}

fn set_previews_visible(app: &AppHandle, visible: bool) {
    app.state::<preview::Preview>().send(preview::Command::Visible(visible));
    app.state::<canvas_preview::CanvasPreview>().send(canvas_preview::Command::Visible(visible));
    app.state::<layout_preview::LayoutPreview>().send(layout_preview::Command::Visible(visible));
}
