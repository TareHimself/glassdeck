//! Wallpaper Engine scenes, rendered live by Wallpaper Engine itself in a hidden window of its own.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use glassdeck_wallpaper_engine::Item;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowLongPtrW,
    SetWindowPos, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};
use windows_capture::window::Window;

use crate::audio_mute::ProcessMuted;
use crate::desktop::{EncoderKind, capture_live};
use crate::pipeline::{EncodedFrame, EncoderSettings, FrameSource, Timing, VideoFormat};

const WINDOW_APPEARS_WITHIN: Duration = Duration::from_secs(15);
const WINDOW_POLL: Duration = Duration::from_millis(100);
/// Far outside any monitor: Wallpaper Engine keeps rendering there, and nobody sees it.
const HIDDEN_AT: (i32, i32) = (-20_000, -20_000);

pub struct SceneSource {
    engine: PathBuf,
    project: PathBuf,
    window_name: String,
    format: VideoFormat,
    settings: EncoderSettings,
    duration: Option<Duration>,
}

impl SceneSource {
    pub fn new(item: &Item, settings: EncoderSettings, duration: Option<Duration>) -> Result<Self> {
        let format = VideoFormat {
            codec: settings.codec,
            width: u16::try_from(settings.width).context("scene too wide")?,
            height: u16::try_from(settings.height).context("scene too tall")?,
        };
        Ok(Self {
            engine: engine_exe(&item.dir)?,
            project: item.dir.join("project.json"),
            window_name: format!("Glassdeck scene {}", item.id),
            format,
            settings,
            duration,
        })
    }
}

impl FrameSource for SceneSource {
    fn format(&self) -> VideoFormat {
        self.format
    }

    fn timing(&self) -> Timing {
        Timing::Live
    }

    fn run(self: Box<Self>, out: SyncSender<EncodedFrame>) -> Result<()> {
        let engine = Engine { exe: &self.engine };
        let scene = engine.open(&self.project, &self.window_name, (self.settings.width, self.settings.height))?;
        // The keyboard has no speakers. Wallpaper Engine's own mute skips windowed wallpapers, so mute its process instead.
        let _muted = ProcessMuted::start(scene.window.process_id().context("finding Wallpaper Engine's process")?);
        capture_live(Window::from_raw_hwnd(scene.window.as_raw_hwnd()), EncoderKind::Gpu, self.settings, self.duration, out)
    }
}

/// Wallpaper Engine's command line; it only works while Wallpaper Engine is running.
struct Engine<'a> {
    exe: &'a Path,
}

impl Engine<'_> {
    fn control(&self, args: &[&str]) -> Result<()> {
        let status = Command::new(self.exe)
            .arg("-control")
            .args(args)
            .status()
            .with_context(|| format!("running {}", self.exe.display()))?;
        ensure!(status.success(), "Wallpaper Engine refused -control {} ({status})", args.join(" "));
        Ok(())
    }

    fn open(&self, project: &Path, name: &str, size: (u32, u32)) -> Result<OpenScene<'_>> {
        let (width, height) = (size.0.to_string(), size.1.to_string());
        let project = project.to_str().context("scene path is not UTF-8")?;
        self.control(&["openWallpaper", "-file", project, "-playInWindow", name, "-width", &width, "-height", &height])?;
        let scene = OpenScene { engine: self, name: name.to_owned(), window: find_window(name)? };
        hide(scene.window, size)?;
        Ok(scene)
    }
}

struct OpenScene<'a> {
    engine: &'a Engine<'a>,
    name: String,
    window: Window,
}

impl Drop for OpenScene<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.engine.control(&["closeWallpaper", "-location", &self.name]) {
            eprintln!("error: closing the scene window: {e:#}");
        }
    }
}

fn find_window(name: &str) -> Result<Window> {
    let begun = Instant::now();
    loop {
        if let Ok(window) = Window::from_name(name) {
            return Ok(window);
        }
        if begun.elapsed() > WINDOW_APPEARS_WITHIN {
            bail!("Wallpaper Engine opened no window named {name:?}; is it running?");
        }
        thread::sleep(WINDOW_POLL);
    }
}

/// Strips the frame so the window is exactly the scene, keeps it off the taskbar, and moves it off every monitor.
fn hide(window: Window, size: (u32, u32)) -> Result<()> {
    let hwnd = HWND(window.as_raw_hwnd());
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex & !(WS_EX_APPWINDOW.0 as isize)) | WS_EX_TOOLWINDOW.0 as isize);
        SetWindowPos(
            hwnd,
            None,
            HIDDEN_AT.0,
            HIDDEN_AT.1,
            size.0 as i32,
            size.1 as i32,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )
        .context("moving the scene window out of sight")?;
    }
    Ok(())
}

/// Steam keeps workshop items in the library Wallpaper Engine is installed in.
fn engine_exe(item_dir: &Path) -> Result<PathBuf> {
    let steamapps = item_dir
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name.eq_ignore_ascii_case("steamapps")))
        .with_context(|| format!("{} is not inside a Steam library", item_dir.display()))?;
    let exe = steamapps.join("common").join("wallpaper_engine").join("wallpaper64.exe");
    ensure!(exe.is_file(), "Wallpaper Engine is not at {}", exe.display());
    Ok(exe)
}
