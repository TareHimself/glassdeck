//! One placed widget: its own Luau VM running `keyboard.luau`, or the error that stopped it.
//! The script registers its callbacks on the injected `widget` table and keeps its state in its own locals.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fontdue::Font;
use glassdeck_luau::placement::{Placement, PlacedWidget};
use glassdeck_luau::{LiveName, LuaResultExt, Print, ResourceRef, Vm, decode, encode};
use glassdeck_package::{Package, ResourceKind};
use mlua::{Function, Table, Value};

use crate::Installed;
use crate::draw::{self, Align, Area, Color, Command, ResourceCache};
use crate::text::{Fit, Overflow, Scrolls};
use crate::live::LiveCache;
use crate::ui::Ui;

const ERROR_FILL: &str = "#4a0d0de0";
const ERROR_STROKE: &str = "#ff6b6b";
const ERROR_TEXT_SIZE: f32 = 12.0;
/// Scrolling text redraws the tile at most this often, since it's rasterised on the CPU.
const SCROLL_FRAME: Duration = Duration::from_millis(33);

pub struct Instance {
    pub label: String,
    pub package: String,
    pub placement: Placement,
    pub installed: Installed,
    run: Run,
    images: ResourceCache,
    /// What the last `on_draw` drew, redrawn without running it while text scrolls.
    drawn: Vec<Command>,
    scrolls: Scrolls,
    scrolling: bool,
    last_scroll_frame: Option<Instant>,
    /// The error tile still has to be drawn.
    error_unshown: bool,
    measured: bool,
}

enum Run {
    Running(Live),
    Failed(String),
}

/// What the script registered with `widget.on_draw(...)` and the rest.
#[derive(Default)]
struct Callbacks {
    on_draw: Option<Function>,
    on_data: Option<Function>,
    on_resize: Option<Function>,
    on_unload: Option<Function>,
}

const CALLBACKS: [&str; 4] = ["on_draw", "on_data", "on_resize", "on_unload"];

/// Field order matters: the Lua values drop before the VM that owns them.
struct Live {
    callbacks: Rc<RefCell<Callbacks>>,
    ui: Ui,
    vm: Vm,
}

pub struct DrawnTile {
    pub pixels: Vec<u32>,
}

impl Instance {
    /// Never fails: a module that can't start shows its error in its own rectangle. `print` takes what
    /// the script prints, instead of the process's own output.
    pub fn start(index: usize, widget: &PlacedWidget, installed: Installed, saved: Option<Vec<u8>>, print: Option<Print>) -> Self {
        let label = format!("widget {index} ({})", widget.package);
        let run = match Live::start(&installed.package, widget, saved, print) {
            Ok(live) => Run::Running(live),
            Err(e) => failed(&label, e),
        };
        Self {
            error_unshown: matches!(run, Run::Failed(_)),
            label,
            package: widget.package.to_string(),
            placement: widget.placement,
            installed,
            run,
            images: ResourceCache::new(),
            drawn: Vec::new(),
            scrolls: Scrolls::default(),
            scrolling: false,
            last_scroll_frame: None,
            measured: false,
        }
    }

    pub fn on_data(&mut self, payload: &[u8]) {
        self.with_live(|live| {
            let data = decode(live.vm.lua(), payload)?;
            let on_data = live.callbacks.borrow().on_data.clone().context("got data from its host half but never called widget.on_data")?;
            live.vm.call::<()>(&on_data, data).context("in widget.on_data")?;
            live.ui.request_redraw();
            Ok(())
        });
    }

    /// Live resources the widget host stored or dropped for this instance; it redraws with them.
    pub fn live_changed(&mut self, names: &[LiveName]) {
        for name in names {
            self.images.forget(&ResourceRef::Live(name.clone()));
        }
        self.with_live(|live| {
            live.ui.request_redraw();
            Ok(())
        });
    }

    pub fn resize(&mut self, placement: Placement) {
        self.placement = placement;
        self.images = ResourceCache::new();
        self.with_live(|live| {
            live.ui.resize(placement.w, placement.h);
            live.ui.request_redraw();
            let on_resize = live.callbacks.borrow().on_resize.clone();
            match on_resize {
                Some(on_resize) => live.vm.call::<()>(&on_resize, (placement.w, placement.h)).context("in widget.on_resize"),
                None => Ok(()),
            }
        });
        self.error_unshown = true;
    }

    pub fn request_redraw(&mut self) {
        self.with_live(|live| {
            live.ui.request_redraw();
            Ok(())
        });
        self.error_unshown = true;
    }

    pub fn move_to(&mut self, placement: Placement) {
        self.placement = placement;
        self.with_live(|live| {
            live.ui.request_redraw();
            Ok(())
        });
        self.error_unshown = true;
    }

    /// What `unload` handed back, encoded for the next VM's `init`.
    pub fn unload(self) -> Option<Vec<u8>> {
        let Run::Running(live) = self.run else { return None };
        let result = (|| -> Result<Option<Vec<u8>>> {
            let on_unload = live.callbacks.borrow().on_unload.clone();
            let Some(on_unload) = on_unload else { return Ok(None) };
            let saved: Value = live.vm.call(&on_unload, ()).context("in widget.on_unload")?;
            if saved.is_nil() { Ok(None) } else { encode(saved).map(Some) }
        })();
        result.unwrap_or_else(|e| {
            eprintln!("error: {}: unload: {e:#}", self.label);
            None
        })
    }

    pub fn tick(&mut self, now: Instant, frame_dt: Option<Duration>) {
        self.with_live(|live| {
            live.vm.run_due_timers(now)?;
            if let Some(dt) = frame_dt {
                for frame in live.ui.take_frames() {
                    live.vm.call::<()>(&frame, dt.as_secs_f64())?;
                    live.ui.request_redraw();
                }
            }
            Ok(())
        });
    }

    pub fn next_timer(&self) -> Option<Instant> {
        match &self.run {
            Run::Running(live) => live.vm.next_timer(),
            Run::Failed(_) => None,
        }
    }

    /// The error that stopped the script, if one did.
    pub fn error(&self) -> Option<&str> {
        match &self.run {
            Run::Failed(message) => Some(message),
            Run::Running(_) => None,
        }
    }

    pub fn wants_frame(&self) -> bool {
        matches!(&self.run, Run::Running(live) if live.ui.wants_frame() || self.scrolling)
    }

    /// The instance's new pixels, if anything asked for a redraw.
    pub fn draw(&mut self, font: &Font, resources: &LiveCache) -> Option<DrawnTile> {
        let size = (self.placement.w, self.placement.h);
        let now = Instant::now();
        let pan_due = self.scrolling && self.last_scroll_frame.is_none_or(|at| now.saturating_duration_since(at) >= SCROLL_FRAME);
        let rerun = matches!(&self.run, Run::Running(live) if live.ui.take_redraw());
        if let Run::Running(live) = &self.run
            && (rerun || pan_due)
        {
            let started = Instant::now();
            let commands = if rerun { live.draw() } else { Ok(std::mem::take(&mut self.drawn)) };
            let result = commands.map(|commands| self.drawn = commands).and_then(|()| {
                let listed = started.elapsed();
                let package = Rc::clone(&self.installed.package);
                let read = |resource: &ResourceRef| match resource {
                    ResourceRef::Packaged(packaged) => package.resource(packaged).map(Some),
                    ResourceRef::Live(name) => Ok(resources.get(name)),
                };
                let tile = draw::render(&self.drawn, size, font, &mut self.images, &read, &mut self.scrolls, now)?;
                self.scrolling = self.scrolls.finish_render();
                self.last_scroll_frame = Some(now);
                Ok((tile, listed))
            });
            match result {
                Ok((tile, listed)) => {
                    if !self.measured {
                        self.measured = true;
                        let total = started.elapsed();
                        let memory_kb = live.vm.used_memory() / 1024;
                        println!(
                            "{}: first draw {:.2} ms in Luau + {:.2} ms rasterising, VM {memory_kb} KB",
                            self.label,
                            ms(listed),
                            ms(total - listed)
                        );
                    }
                    return Some(DrawnTile { pixels: draw::to_argb(&tile) });
                }
                Err(e) => {
                    self.run = failed(&self.label, e);
                    self.error_unshown = true;
                }
            }
        }
        match &self.run {
            Run::Failed(message) if std::mem::take(&mut self.error_unshown) => Some(error_tile(&self.package, message, size, font)),
            _ => None,
        }
    }

    fn with_live(&mut self, act: impl FnOnce(&mut Live) -> Result<()>) {
        if let Run::Running(live) = &mut self.run
            && let Err(e) = act(live)
        {
            self.run = failed(&self.label, e);
            self.error_unshown = true;
        }
    }
}

impl Live {
    fn start(package: &Rc<Package>, widget: &PlacedWidget, saved: Option<Vec<u8>>, print: Option<Print>) -> Result<Self> {
        let source = package.keyboard_script()?.with_context(|| format!("{} has no keyboard.luau", package.id()))?;
        let vm = Vm::new()?;
        if let Some(print) = print {
            vm.install_print(print)?;
        }
        let ui = Ui::new(widget.placement.w, widget.placement.h);
        let readable = Rc::clone(package);
        vm.install_resources(&[ResourceKind::Keyboard], Rc::new(move |resource| readable.resource(resource)))?;
        vm.install_live_resources()?;
        vm.seal()?;
        let saved = saved.map(|bytes| decode(vm.lua(), &bytes)).transpose()?.unwrap_or(Value::Nil);
        let callbacks = Rc::new(RefCell::new(Callbacks::default()));
        let api = widget_api(&vm, &ui, &callbacks, saved)?;
        vm.lua().globals().set("widget", api).lua_context("injecting widget")?;
        vm.load::<()>(&source, "keyboard.luau")?;
        if callbacks.borrow().on_draw.is_none() {
            bail!("keyboard.luau must register a draw function with widget.on_draw");
        }
        Ok(Self { callbacks, ui, vm })
    }

    fn draw(&self) -> Result<Vec<Command>> {
        let on_draw = self.callbacks.borrow().on_draw.clone().context("widget.on_draw was never called")?;
        self.ui.begin_draw();
        let result = self.vm.call::<()>(&on_draw, self.ui.clone());
        let commands = self.ui.end_draw();
        result.context("in widget.on_draw")?;
        Ok(commands)
    }
}

/// The `widget` table `keyboard.luau` sees: one-time registration of each callback, `saved`, and the
/// redraw triggers.
fn widget_api(vm: &Vm, ui: &Ui, callbacks: &Rc<RefCell<Callbacks>>, saved: Value) -> Result<Table> {
    let lua = vm.lua();
    let api = lua.create_table().lua_context("creating widget")?;
    for name in CALLBACKS {
        let registered = Rc::clone(callbacks);
        let register = lua
            .create_function(move |_, callback: Function| {
                let mut callbacks = registered.borrow_mut();
                let slot = match name {
                    "on_draw" => &mut callbacks.on_draw,
                    "on_data" => &mut callbacks.on_data,
                    "on_resize" => &mut callbacks.on_resize,
                    _ => &mut callbacks.on_unload,
                };
                if slot.is_some() {
                    return Err(mlua::Error::runtime(format!("widget.{name} was already called; register one function")));
                }
                *slot = Some(callback);
                Ok(())
            })
            .lua_context("creating widget")?;
        api.raw_set(name, register).lua_context("creating widget")?;
    }
    api.raw_set("every", vm.every_function()?).lua_context("creating widget.every")?;
    let redrawn = ui.clone();
    let redraw = lua
        .create_function(move |_, ()| {
            redrawn.request_redraw();
            Ok(())
        })
        .lua_context("creating widget.redraw")?;
    api.raw_set("redraw", redraw).lua_context("creating widget.redraw")?;
    let framed = ui.clone();
    let request_frame = lua
        .create_function(move |_, callback: Function| {
            framed.request_frame(callback);
            Ok(())
        })
        .lua_context("creating widget.request_frame")?;
    api.raw_set("request_frame", request_frame).lua_context("creating widget.request_frame")?;
    api.raw_set("saved", saved).lua_context("creating widget.saved")?;
    Ok(api)
}

fn failed(label: &str, error: anyhow::Error) -> Run {
    eprintln!("error: {label}: {error:#}");
    Run::Failed(format!("{error:#}"))
}

fn error_tile(module: &str, message: &str, (w, h): (u32, u32), font: &Font) -> DrawnTile {
    let color = |hex: &str| Color::parse(hex).expect("error tile colors are valid");
    let (w, h) = (w as f32, h as f32);
    let mut commands = vec![
        Command::Rect {
            area: Area { x: 1.0, y: 1.0, w: w - 2.0, h: h - 2.0 },
            fill: Some(color(ERROR_FILL)),
            stroke: Some((color(ERROR_STROKE), 2.0)),
            radius: 8.0,
        },
        Command::Text {
            text: module.to_owned(),
            x: 8.0,
            y: 6.0,
            size: 14.0,
            color: Color::WHITE,
            align: Align::Left,
            fit: Some(Fit { width: w - 16.0, overflow: Overflow::Ellipsis }),
            font: None,
        },
    ];
    commands.push(Command::Text {
        text: message.to_owned(),
        x: 8.0,
        y: 26.0,
        size: ERROR_TEXT_SIZE,
        color: color(ERROR_STROKE),
        align: Align::Left,
        fit: Some(Fit { width: w - 16.0, overflow: Overflow::Wrap { lines: None, line_height: 1.25, height: Some(h - 32.0) } }),
        font: None,
    });
    let no_assets = |resource: &ResourceRef| -> Result<Option<Vec<u8>>> { anyhow::bail!("the error tile has no {resource}") };
    let tile = draw::render(&commands, (w as u32, h as u32), font, &mut ResourceCache::new(), &no_assets, &mut Scrolls::default(), Instant::now())
        .expect("the error tile draws");
    DrawnTile { pixels: draw::to_argb(&tile) }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use glassdeck_package::pack_folder;

    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn module(name: &str, script: &str) -> (Scratch, Installed) {
        module_with(name, script, &[])
    }

    /// A packed module whose folder also has `files`, as (path under assets/, contents).
    fn module_with(name: &str, script: &str, files: &[(&str, &[u8])]) -> (Scratch, Installed) {
        let dir = std::env::temp_dir().join(format!("glassdeck-widget-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        for (path, contents) in files {
            let path = dir.join("src/assets").join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        fs::write(
            dir.join("src/deck.json"),
            r#"{ "id": "local.me.test", "title": "Test", "version": "1.0.0", "type": "widget", "details": { "size": { "w": 64, "h": 32 } } }"#,
        )
        .unwrap();
        fs::write(dir.join("src/keyboard.luau"), script).unwrap();
        let package = pack_folder(&dir.join("src"), &dir.join("widget.deck")).unwrap();
        let package = package.write_keyboard_copy(&dir.join("keyboard.deck")).unwrap();
        let modified = fs::metadata(package.path().unwrap()).unwrap().modified().unwrap();
        (Scratch(dir), Installed { package: Rc::new(package), modified })
    }

    fn widget() -> PlacedWidget {
        PlacedWidget { package: glassdeck_package::QualifiedId::parse("widget.local.me.test").unwrap(), placement: Placement { x: 0, y: 0, w: 64, h: 32 } }
    }

    fn font() -> Font {
        crate::load_font(None).unwrap()
    }

    fn opaque_pixels(tile: &DrawnTile) -> usize {
        tile.pixels.iter().filter(|p| *p >> 24 == 0xFF).count()
    }

    #[test]
    fn draws_once_then_only_when_asked() {
        let script = r##"
            local n = 3
            widget.on_data(function(data) n = data.n end)
            widget.on_draw(function(ui) ui:rect(0, 0, ui.width / 2, ui.height, { fill = "#ff0000" }) end)
        "##;
        let (_scratch, installed) = module("draws", script);
        let mut instance = Instance::start(0, &widget(), installed, None, None);
        let first = instance.draw(&font(), &LiveCache::default()).expect("the first draw");
        assert_eq!(opaque_pixels(&first), 32 * 32);
        assert!(instance.draw(&font(), &LiveCache::default()).is_none());

        instance.on_data(&rmpv_map_n(5));
        assert!(instance.draw(&font(), &LiveCache::default()).is_some());
    }

    #[test]
    fn saved_state_survives_a_reload_and_misuse_fails_loudly() {
        let script = r##"
            local count = (widget.saved or 0) + 1
            widget.on_unload(function() return count end)
            widget.on_draw(function(ui) ui:rect(0, 0, count, 1, { fill = "#ffffff" }) end)
        "##;
        let (_scratch, installed) = module("saved", script);
        let first = Instance::start(0, &widget(), installed.clone(), None, None);
        let saved = first.unload();
        let mut second = Instance::start(0, &widget(), installed, saved, None);
        let tile = second.draw(&font(), &LiveCache::default()).expect("the first draw");
        assert_eq!(opaque_pixels(&tile), 2, "count was 1, then 2 after the reload");

        let (_scratch, installed) = module("no-draw", "widget.on_data(function() end)");
        let instance = Instance::start(0, &widget(), installed, None, None);
        assert!(matches!(instance.run, Run::Failed(ref m) if m.contains("widget.on_draw")));

        let twice = "widget.on_draw(function() end)\nwidget.on_draw(function() end)";
        let (_scratch, installed) = module("twice", twice);
        let instance = Instance::start(0, &widget(), installed, None, None);
        assert!(matches!(instance.run, Run::Failed(ref m) if m.contains("already called")));

        let timers = "widget.on_draw(function() end)\nwidget.every(1, function() widget.redraw() end)";
        let (_scratch, installed) = module("timer", timers);
        let instance = Instance::start(0, &widget(), installed, None, None);
        assert!(matches!(instance.run, Run::Running(_)));
        assert!(instance.next_timer().is_some());
    }

    #[test]
    fn draws_an_image_from_a_keyboard_resource() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="#00ff00"/></svg>"##;
        let script = r##"
            local dot = keyboard.resource("icons/dot.svg")
            widget.on_draw(function(ui) ui:image(dot, { x = 0, y = 0, w = 16, h = 16 }) end)
        "##;
        let (_scratch, installed) = module_with("image", script, &[("keyboard/icons/dot.svg", svg.as_bytes())]);
        let mut instance = Instance::start(0, &widget(), installed, None, None);
        let tile = instance.draw(&font(), &LiveCache::default()).expect("the first draw");
        assert!(matches!(instance.run, Run::Running(_)));
        assert_eq!(opaque_pixels(&tile), 16 * 16);
    }

    #[test]
    fn draws_text_in_a_font_the_module_bundles() {
        let script = r##"
            local noto = keyboard.resource("fonts/noto.otf")
            widget.on_draw(function(ui) ui:text("漢A", { size = 24, font = noto }) end)
        "##;
        let noto = include_bytes!("../fonts/NotoSansJP-Bold.otf");
        let (_scratch, installed) = module_with("font", script, &[("keyboard/fonts/noto.otf", noto)]);
        let mut instance = Instance::start(0, &widget(), installed, None, None);
        let tile = instance.draw(&font(), &LiveCache::default()).expect("the first draw");
        assert!(matches!(instance.run, Run::Running(_)));
        assert!(opaque_pixels(&tile) > 0);
    }

    #[test]
    fn a_font_that_isnt_one_becomes_an_error_tile() {
        let script = r##"
            local bad = keyboard.resource("fonts/bad.ttf")
            widget.on_draw(function(ui) ui:text("A", { font = bad }) end)
        "##;
        let (_scratch, installed) = module_with("bad-font", script, &[("keyboard/fonts/bad.ttf", b"not a font")]);
        let mut instance = Instance::start(0, &widget(), installed, None, None);
        instance.draw(&font(), &LiveCache::default()).expect("an error tile");
        assert!(
            matches!(instance.run, Run::Failed(ref m) if m.contains("as a font")),
            "{:?}",
            match &instance.run {
                Run::Failed(m) => m.as_str(),
                Run::Running(_) => "running",
            }
        );
    }

    #[test]
    fn draws_a_live_resource_once_it_arrives() {
        let script = r##"
            local cover = live.resource("cover")
            widget.on_draw(function(ui) ui:image(cover, { x = 0, y = 0, w = 16, h = 16 }) end)
        "##;
        let (_scratch, installed) = module("live", script);
        let mut instance = Instance::start(0, &widget(), installed, None, None);
        let mut live = LiveCache::default();
        assert_eq!(opaque_pixels(&instance.draw(&font(), &live).expect("the first draw")), 0);

        let cover = LiveName::parse("cover").unwrap();
        let mut green = resvg::tiny_skia::Pixmap::new(4, 4).unwrap();
        green.fill(resvg::tiny_skia::Color::from_rgba8(0, 255, 0, 255));
        live.put(cover.clone(), green.encode_png().unwrap()).unwrap();
        instance.live_changed(std::slice::from_ref(&cover));
        let tile = instance.draw(&font(), &live).expect("a redraw for the new resource");
        assert!(matches!(instance.run, Run::Running(_)));
        assert_eq!(opaque_pixels(&tile), 16 * 16);

        live.put(cover.clone(), Vec::new()).unwrap();
        instance.live_changed(&[cover]);
        assert_eq!(opaque_pixels(&instance.draw(&font(), &live).expect("a redraw after removal")), 0);
    }

    #[test]
    fn an_endless_draw_becomes_an_error_tile() {
        let script = "widget.on_draw(function() while true do end end)";
        let (_scratch, installed) = module("endless", script);
        let mut instance = Instance::start(0, &widget(), installed, None, None);
        let tile = instance.draw(&font(), &LiveCache::default()).expect("an error tile");
        assert!(tile.pixels.iter().any(|p| *p != 0));
        assert!(instance.draw(&font(), &LiveCache::default()).is_none());
        assert!(matches!(instance.run, Run::Failed(ref m) if m.contains("took longer than")));
    }

    #[test]
    fn drawing_outside_on_draw_is_an_error() {
        let script = r#"
            local kept
            widget.on_draw(function(ui) kept = ui end)
            widget.on_data(function() kept:text("hi") end)
        "#;
        let (_scratch, installed) = module("outside", script);
        let mut instance = Instance::start(0, &widget(), installed, None, None);
        instance.draw(&font(), &LiveCache::default());
        instance.on_data(&rmpv_map_n(1));
        assert!(matches!(instance.run, Run::Failed(ref m) if m.contains("only works inside widget.on_draw")));
    }

    #[test]
    fn unknown_options_are_refused() {
        let script = "widget.on_draw(function(ui) ui:text('hi', { colour = '#fff' }) end)";
        let (_scratch, installed) = module("typo", script);
        let mut instance = Instance::start(0, &widget(), installed, None, None);
        instance.draw(&font(), &LiveCache::default());
        assert!(matches!(instance.run, Run::Failed(ref m) if m.contains("unknown option colour")));
    }

    #[test]
    fn frame_requests_redraw_until_they_stop() {
        let script = r#"
            local frames = 0
            local function step(dt)
                frames += 1
                if frames < 2 then widget.request_frame(step) end
            end
            widget.request_frame(step)
            widget.on_draw(function(ui) end)
        "#;
        let (_scratch, installed) = module("frames", script);
        let mut instance = Instance::start(0, &widget(), installed, None, None);
        instance.draw(&font(), &LiveCache::default());
        let now = Instant::now();
        for expected in [true, true, false] {
            assert_eq!(instance.wants_frame(), expected);
            instance.tick(now, Some(Duration::from_millis(16)));
            assert_eq!(instance.draw(&font(), &LiveCache::default()).is_some(), expected);
        }
    }

    #[test]
    fn scrolling_text_keeps_asking_for_frames_and_short_text_does_not() {
        for (title, scrolls) in [("a title far too long for a sixty four pixel tile", true), ("hi", false)] {
            let script = format!("widget.on_draw(function(ui) ui:text({title:?}, {{ w = 64, overflow = 'scroll' }}) end)");
            let (_scratch, installed) = module("scroll", &script);
            let mut instance = Instance::start(0, &widget(), installed, None, None);
            assert!(instance.draw(&font(), &LiveCache::default()).is_some());
            assert_eq!(instance.wants_frame(), scrolls, "{title}");
        }
    }

    #[test]
    fn text_options_that_dont_go_together_are_errors() {
        for (options, says) in [
            ("{ overflow = 'wrap' }", "overflow needs w"),
            ("{ w = 40, lines = 2 }", "lines only works with overflow = \"wrap\""),
            ("{ w = 40, overflow = 'scroll', min_size = 8 }", "min_size only works with overflow = \"shrink\""),
            ("{ w = 40, overflow = 'fade' }", "is not ellipsis, shrink, wrap, scroll or clip"),
        ] {
            let script = format!("widget.on_draw(function(ui) ui:text('x', {options}) end)");
            let (_scratch, installed) = module("text-options", &script);
            let mut instance = Instance::start(0, &widget(), installed, None, None);
            instance.draw(&font(), &LiveCache::default());
            assert!(instance.error().is_some_and(|e| e.contains(says)), "{options}: {:?}", instance.error());
        }
    }

    #[test]
    fn print_goes_where_it_is_sent_and_errors_can_be_read() {
        let printed = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&printed);
        let print: Print = Rc::new(move |line| sink.borrow_mut().push(line));
        let script = "print('hello', 2, true)
widget.on_draw(function() error('boom') end)";
        let (_scratch, installed) = module("print", script);
        let mut instance = Instance::start(0, &widget(), installed, None, Some(print));
        assert_eq!(*printed.borrow(), ["hello	2	true"]);
        assert_eq!(instance.error(), None);
        instance.draw(&font(), &LiveCache::default());
        assert!(instance.error().is_some_and(|e| e.contains("boom")));
    }

    #[test]
    fn every_example_widget_starts_and_draws() {
        let examples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../modules");
        let mut seen = 0;
        for entry in fs::read_dir(&examples).unwrap() {
            let folder = entry.unwrap().path();
            if !folder.join("keyboard.luau").is_file() {
                continue;
            }
            let out = std::env::temp_dir().join(format!("glassdeck-example-{}-{}.deck", seen, std::process::id()));
            let package = pack_folder(&folder, &out).unwrap();
            let id = package.id().clone();
            let modified = fs::metadata(&out).unwrap().modified().unwrap();
            let placed = PlacedWidget { package: id, placement: Placement { x: 0, y: 0, w: 320, h: 160 } };
            let mut instance = Instance::start(0, &placed, Installed { package: Rc::new(package), modified }, None, None);
            instance.draw(&font(), &LiveCache::default());
            assert!(matches!(instance.run, Run::Running(_)), "{} doesn't run", folder.display());
            fs::remove_file(&out).unwrap();
            seen += 1;
        }
        assert_eq!(seen, 4);
    }

    fn rmpv_map_n(n: u8) -> Vec<u8> {
        vec![0x81, 0xa1, b'n', n]
    }
}
