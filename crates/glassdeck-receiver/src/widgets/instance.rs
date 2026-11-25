//! One placed widget: its own Luau VM running `keyboard.luau`, or the error that stopped it.

use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fontdue::Font;
use glassdeck_package::ModulePackage;
use glassdeck_luau::profile::{Placement, Widget};
use glassdeck_luau::{LuaResultExt, Vm, decode, encode, from_json};
use mlua::{Function, Table, Value};

use super::Installed;
use super::draw::{self, Align, Area, Color, Command, Images};
use super::ui::Ui;

const ERROR_FILL: &str = "#4a0d0de0";
const ERROR_STROKE: &str = "#ff6b6b";
const ERROR_TEXT_SIZE: f32 = 12.0;
/// Rough glyph width at the error text size, for wrapping without measuring.
const ERROR_CHAR_WIDTH: f32 = 7.0;

pub struct Instance {
    pub label: String,
    pub module: String,
    pub placement: Placement,
    pub settings: serde_json::Value,
    pub installed: Installed,
    run: Run,
    images: Images,
    /// The error tile still has to be drawn.
    error_unshown: bool,
    measured: bool,
}

enum Run {
    Running(Live),
    Failed(String),
}

/// Field order matters: the Lua values drop before the VM that owns them.
struct Live {
    hooks: Table,
    state: Value,
    ui: Ui,
    vm: Vm,
}

pub struct DrawnTile {
    pub pixels: Vec<u32>,
}

impl Instance {
    /// Never fails: a module that can't start shows its error in its own rectangle.
    pub fn start(index: usize, widget: &Widget, installed: Installed, saved: Option<Vec<u8>>) -> Self {
        let label = format!("widget {index} ({})", widget.module);
        let run = match Live::start(&installed.package, widget, saved) {
            Ok(live) => Run::Running(live),
            Err(e) => failed(&label, e),
        };
        Self {
            error_unshown: matches!(run, Run::Failed(_)),
            label,
            module: widget.module.clone(),
            placement: widget.placement,
            settings: widget.settings.clone(),
            installed,
            run,
            images: Images::new(),
            measured: false,
        }
    }

    pub fn on_data(&mut self, payload: &[u8]) {
        self.with_live(|live| {
            let data = decode(live.vm.lua(), payload)?;
            let hook = live.hook("on_data")?.context("got data from its PC half but has no on_data")?;
            if live.vm.call::<bool>(&hook, (live.state.clone(), data))? {
                live.ui.request_redraw();
            }
            Ok(())
        });
    }

    pub fn resize(&mut self, placement: Placement) {
        self.placement = placement;
        self.images = Images::new();
        self.with_live(|live| {
            live.ui.resize(placement.w, placement.h);
            live.ui.request_redraw();
            match live.hook("resize")? {
                Some(hook) => live.vm.call::<()>(&hook, (live.state.clone(), placement.w, placement.h)),
                None => Ok(()),
            }
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

    pub fn settings_changed(&mut self, settings: serde_json::Value) {
        self.with_live(|live| {
            live.ui.request_redraw();
            match live.hook("settings_changed")? {
                Some(hook) => live.vm.call::<()>(&hook, (live.state.clone(), from_json(live.vm.lua(), &settings)?)),
                None => Ok(()),
            }
        });
        self.settings = settings;
    }

    /// What `unload` handed back, encoded for the next VM's `init`.
    pub fn unload(self) -> Option<Vec<u8>> {
        let Run::Running(live) = self.run else { return None };
        let result = (|| -> Result<Option<Vec<u8>>> {
            let Some(hook) = live.hook("unload")? else { return Ok(None) };
            let saved: Value = live.vm.call(&hook, live.state.clone())?;
            if saved.is_nil() { Ok(None) } else { encode(live.vm.lua(), saved).map(Some) }
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

    pub fn wants_frame(&self) -> bool {
        matches!(&self.run, Run::Running(live) if live.ui.wants_frame())
    }

    /// The instance's new pixels, if anything asked for a redraw.
    pub fn draw(&mut self, font: &Font) -> Option<DrawnTile> {
        let size = (self.placement.w, self.placement.h);
        if let Run::Running(live) = &self.run
            && live.ui.take_redraw()
        {
            let started = Instant::now();
            let result = live.draw().and_then(|commands| {
                let listed = started.elapsed();
                let package = Rc::clone(&self.installed.package);
                let tile = draw::render(&commands, size, font, &mut self.images, &|name| package.asset(name))?;
                Ok((tile, listed))
            });
            match result {
                Ok((tile, listed)) => {
                    if !self.measured {
                        self.measured = true;
                        let total = started.elapsed();
                        let memory_kb = live.vm.used_memory() / 1024;
                        println!("{}: first draw {:.2} ms in Luau + {:.2} ms rasterising, VM {memory_kb} KB", self.label, ms(listed), ms(total - listed));
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
            Run::Failed(message) if std::mem::take(&mut self.error_unshown) => Some(error_tile(&self.module, message, size, font)),
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
    fn start(package: &ModulePackage, widget: &Widget, saved: Option<Vec<u8>>) -> Result<Self> {
        let source = package.keyboard_script()?;
        let vm = Vm::new()?;
        let ui = Ui::new(widget.placement.w, widget.placement.h, vm.every_function()?);
        let lua = vm.lua();
        lua.globals().set("ui", ui.clone()).lua_context("installing ui")?;
        vm.seal()?;
        let hooks: Table = vm.load(&source, "keyboard.luau")?;
        hooks.get::<Function>("draw").lua_context("keyboard.luau must return a table with a draw function")?;
        let settings = from_json(lua, &widget.settings)?;
        let saved = saved.map(|bytes| decode(lua, &bytes)).transpose()?.unwrap_or(Value::Nil);
        let state = match hooks.get::<Option<Function>>("init").lua_context("reading init")? {
            Some(init) => vm.call(&init, (settings, saved)).context("init")?,
            None => Value::Nil,
        };
        Ok(Self { hooks, state, ui, vm })
    }

    fn hook(&self, name: &str) -> Result<Option<Function>> {
        self.hooks.get::<Option<Function>>(name).lua_context(&format!("reading {name}"))
    }

    fn draw(&self) -> Result<Vec<Command>> {
        let draw: Function = self.hooks.get("draw").lua_context("reading draw")?;
        self.ui.begin_draw();
        let result = self.vm.call::<()>(&draw, (self.state.clone(), self.ui.clone()));
        let commands = self.ui.end_draw();
        result.context("draw")?;
        Ok(commands)
    }
}

fn failed(label: &str, error: anyhow::Error) -> Run {
    eprintln!("error: {label}: {error:#}");
    Run::Failed(format!("{error:#}"))
}

fn error_tile(module: &str, message: &str, (w, h): (u32, u32), font: &Font) -> DrawnTile {
    let color = |hex: &str| Color::parse(hex).expect("error tile colors are valid");
    let (w, h) = (w as f32, h as f32);
    let mut commands = vec![
        Command::Rect { area: Area { x: 1.0, y: 1.0, w: w - 2.0, h: h - 2.0 }, fill: Some(color(ERROR_FILL)), stroke: Some((color(ERROR_STROKE), 2.0)), radius: 8.0 },
        Command::Text { text: module.to_owned(), x: 8.0, y: 6.0, size: 14.0, color: Color::WHITE, align: Align::Left, width: Some(w - 16.0) },
    ];
    let per_line = ((w - 16.0) / ERROR_CHAR_WIDTH).max(1.0) as usize;
    let chars: Vec<char> = message.chars().collect();
    for (row, line) in chars.chunks(per_line).enumerate() {
        let y = 26.0 + row as f32 * (ERROR_TEXT_SIZE + 3.0);
        if y + ERROR_TEXT_SIZE > h {
            break;
        }
        let text = line.iter().collect();
        commands.push(Command::Text { text, x: 8.0, y, size: ERROR_TEXT_SIZE, color: color(ERROR_STROKE), align: Align::Left, width: Some(w - 16.0) });
    }
    let no_assets = |name: &str| -> Result<Vec<u8>> { anyhow::bail!("the error tile has no asset {name}") };
    let tile = draw::render(&commands, (w as u32, h as u32), font, &mut Images::new(), &no_assets).expect("the error tile draws");
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
        let dir = std::env::temp_dir().join(format!("glassdeck-widget-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/module.json"), r#"{ "format": 1, "id": "dev.test.widget", "name": "Test", "version": "1.0.0", "size": { "w": 64, "h": 32 } }"#).unwrap();
        fs::write(dir.join("src/keyboard.luau"), script).unwrap();
        let package = pack_folder(&dir.join("src"), &dir.join("widget.deckmod")).unwrap();
        let modified = fs::metadata(package.path()).unwrap().modified().unwrap();
        (Scratch(dir), Installed { package: Rc::new(package), modified })
    }

    fn widget() -> Widget {
        Widget { module: "dev.test.widget".into(), placement: Placement { x: 0, y: 0, w: 64, h: 32 }, settings: serde_json::json!({ "n": 3 }) }
    }

    fn font() -> Font {
        crate::load_font(&crate::platform::DEFAULT_FONT.into()).unwrap()
    }

    fn opaque_pixels(tile: &DrawnTile) -> usize {
        tile.pixels.iter().filter(|p| *p >> 24 == 0xFF).count()
    }

    #[test]
    fn draws_once_then_only_when_asked() {
        let script = r##"
            local module = {}
            function module.init(settings) return { n = settings.n } end
            function module.on_data(state, data) state.n = data.n; return true end
            function module.draw(state, ui) ui:rect(0, 0, ui.width / 2, ui.height, { fill = "#ff0000" }) end
            return module
        "##;
        let (_scratch, installed) = module("draws", script);
        let mut instance = Instance::start(0, &widget(), installed, None);
        let first = instance.draw(&font()).expect("the first draw");
        assert_eq!(opaque_pixels(&first), 32 * 32);
        assert!(instance.draw(&font()).is_none());

        instance.on_data(&rmpv_map_n(5));
        assert!(instance.draw(&font()).is_some());
    }

    #[test]
    fn an_endless_draw_becomes_an_error_tile() {
        let script = "return { draw = function() while true do end end }";
        let (_scratch, installed) = module("endless", script);
        let mut instance = Instance::start(0, &widget(), installed, None);
        let tile = instance.draw(&font()).expect("an error tile");
        assert!(tile.pixels.iter().any(|p| *p != 0));
        assert!(instance.draw(&font()).is_none());
        assert!(matches!(instance.run, Run::Failed(ref m) if m.contains("took longer than")));
    }

    #[test]
    fn drawing_outside_draw_is_an_error() {
        let script = "local m = {}; function m.init() ui:text('hi') end; function m.draw() end; return m";
        let (_scratch, installed) = module("outside", script);
        let instance = Instance::start(0, &widget(), installed, None);
        assert!(matches!(instance.run, Run::Failed(ref m) if m.contains("only works inside draw")));
    }

    #[test]
    fn unknown_options_are_refused() {
        let script = "return { draw = function(_, ui) ui:text('hi', { colour = '#fff' }) end }";
        let (_scratch, installed) = module("typo", script);
        let mut instance = Instance::start(0, &widget(), installed, None);
        instance.draw(&font());
        assert!(matches!(instance.run, Run::Failed(ref m) if m.contains("unknown option colour")));
    }

    #[test]
    fn unload_hands_its_state_to_the_next_init() {
        let script = r#"
            local m = {}
            function m.init(settings, saved) return { count = if saved then saved.count + 1 else 0 } end
            function m.unload(state) return state end
            function m.draw(state, ui) ui:text(tostring(state.count)) end
            return m
        "#;
        let (_scratch, installed) = module("reload", script);
        let first = Instance::start(0, &widget(), installed.clone(), None);
        let saved = first.unload().expect("unload returned state");
        let second = Instance::start(0, &widget(), installed, Some(saved));
        let Run::Running(live) = &second.run else { panic!("second instance failed") };
        let count: f64 = live.state.as_table().unwrap().get("count").unwrap();
        assert_eq!(count, 1.0);
    }

    #[test]
    fn frame_requests_redraw_until_they_stop() {
        let script = r#"
            local m = {}
            function m.init()
                local state = { frames = 0 }
                local function step(dt)
                    state.frames += 1
                    if state.frames < 2 then ui:request_frame(step) end
                end
                ui:request_frame(step)
                return state
            end
            function m.draw(state, ui) end
            return m
        "#;
        let (_scratch, installed) = module("frames", script);
        let mut instance = Instance::start(0, &widget(), installed, None);
        instance.draw(&font());
        let now = Instant::now();
        for expected in [true, true, false] {
            assert_eq!(instance.wants_frame(), expected);
            instance.tick(now, Some(Duration::from_millis(16)));
            assert_eq!(instance.draw(&font()).is_some(), expected);
        }
    }

    fn rmpv_map_n(n: u8) -> Vec<u8> {
        vec![0x81, 0xa1, b'n', n]
    }
}
