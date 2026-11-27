//! The `ui` object `script.on_draw` gets: drawing calls, which only work while it runs.

use std::cell::RefCell;
use std::rc::Rc;

use glassdeck_luau::{ResourceRef, resource_of};
use glassdeck_package::Resource;
use mlua::{AnyUserData, Function, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::draw::{Align, Area, Color, Command};
use crate::text::{Fit, Overflow};

#[derive(Clone)]
pub struct Ui(Rc<RefCell<UiState>>);

struct UiState {
    width: u32,
    height: u32,
    /// Collects commands while `draw` runs; drawing at any other time is an error.
    drawing: Option<Vec<Command>>,
    redraw: bool,
    frames: Vec<Function>,
}

impl Ui {
    pub fn new(width: u32, height: u32) -> Self {
        Self(Rc::new(RefCell::new(UiState { width, height, drawing: None, redraw: true, frames: Vec::new() })))
    }

    /// Runs `callback` once at the next screen refresh, then redraws.
    pub fn request_frame(&self, callback: Function) {
        self.0.borrow_mut().frames.push(callback);
    }

    pub fn resize(&self, width: u32, height: u32) {
        let mut state = self.0.borrow_mut();
        (state.width, state.height) = (width, height);
    }

    pub fn request_redraw(&self) {
        self.0.borrow_mut().redraw = true;
    }

    /// Whether a redraw was asked for since the last call.
    pub fn take_redraw(&self) -> bool {
        std::mem::take(&mut self.0.borrow_mut().redraw)
    }

    pub fn wants_frame(&self) -> bool {
        !self.0.borrow().frames.is_empty()
    }

    pub fn take_frames(&self) -> Vec<Function> {
        std::mem::take(&mut self.0.borrow_mut().frames)
    }

    pub fn begin_draw(&self) {
        self.0.borrow_mut().drawing = Some(Vec::new());
    }

    pub fn end_draw(&self) -> Vec<Command> {
        self.0.borrow_mut().drawing.take().unwrap_or_default()
    }

    fn record(&self, method: &str, command: Command) -> mlua::Result<()> {
        match self.0.borrow_mut().drawing.as_mut() {
            Some(commands) => {
                commands.push(command);
                Ok(())
            }
            None => Err(mlua::Error::runtime(format!("ui:{method} only works inside script.on_draw"))),
        }
    }

    fn size(&self) -> (f32, f32) {
        let state = self.0.borrow();
        (state.width as f32, state.height as f32)
    }
}

impl UserData for Ui {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("width", |_, ui| Ok(ui.0.borrow().width));
        fields.add_field_method_get("height", |_, ui| Ok(ui.0.borrow().height));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("text", |_, ui, (text, opts): (String, Option<Table>)| {
            let o = Opts::new("text", opts, TEXT_OPTIONS)?;
            let align = o.string("align")?.map(|a| Align::parse(&a)).transpose().map_err(o.wrap())?.unwrap_or(Align::Left);
            let command = Command::Text {
                text,
                x: o.number("x", 0.0)?,
                y: o.number("y", 0.0)?,
                size: o.number("size", 16.0)?,
                color: o.color("color")?.unwrap_or(Color::WHITE),
                align,
                fit: o.fit()?,
                font: o.font()?,
            };
            ui.record("text", command)
        });
        methods.add_method("rect", |_, ui, (x, y, w, h, opts): (f32, f32, f32, f32, Option<Table>)| {
            let o = Opts::new("rect", opts, &["fill", "stroke", "stroke_width", "radius"])?;
            let (fill, stroke) = (o.color("fill")?, o.color("stroke")?);
            let stroke = stroke.map(|color| o.number("stroke_width", 2.0).map(|width| (color, width))).transpose()?;
            let fill = if fill.is_none() && stroke.is_none() { Some(Color::WHITE) } else { fill };
            ui.record("rect", Command::Rect { area: Area { x, y, w, h }, fill, stroke, radius: o.number("radius", 0.0)? })
        });
        methods.add_method("bar", |_, ui, (fraction, opts): (f32, Option<Table>)| {
            let o = Opts::new("bar", opts, &["x", "y", "w", "h", "color"])?;
            let command = Command::Bar { fraction, area: o.area(ui.size())?, color: o.color("color")?.unwrap_or(Color::WHITE) };
            ui.record("bar", command)
        });
        methods.add_method("graph", |_, ui, (values, opts): (Vec<f32>, Option<Table>)| {
            let o = Opts::new("graph", opts, &["x", "y", "w", "h", "color", "min", "max"])?;
            let command = Command::Graph {
                values,
                area: o.area(ui.size())?,
                color: o.color("color")?.unwrap_or(Color::WHITE),
                min: o.optional_number("min")?,
                max: o.optional_number("max")?,
            };
            ui.record("graph", command)
        });
        methods.add_method("image", |_, ui, (handle, opts): (AnyUserData, Option<Table>)| {
            let o = Opts::new("image", opts, &["x", "y", "w", "h", "radius"])?;
            ui.record("image", Command::Image { resource: resource_of(&handle)?, area: o.area(ui.size())?, radius: o.number("radius", 0.0)? })
        });
        methods.add_method("arc", |_, ui, (cx, cy, r, from, to, opts): (f32, f32, f32, f32, f32, Option<Table>)| {
            let o = Opts::new("arc", opts, &["width", "color"])?;
            let command =
                Command::Arc { cx, cy, r, from, to, width: o.number("width", 6.0)?, color: o.color("color")?.unwrap_or(Color::WHITE) };
            ui.record("arc", command)
        });
    }
}

const TEXT_OPTIONS: &[&str] =
    &["x", "y", "size", "color", "align", "w", "font", "overflow", "min_size", "lines", "line_height", "h", "speed", "pause"];

/// Options that only mean something with one `overflow`, and that one.
const OVERFLOW_ONLY: [(&str, &str); 6] =
    [("min_size", "shrink"), ("lines", "wrap"), ("line_height", "wrap"), ("h", "wrap"), ("speed", "scroll"), ("pause", "scroll")];

/// A call's options table, checked against the names that call takes so typos fail loudly.
struct Opts {
    method: &'static str,
    table: Option<Table>,
}

impl Opts {
    fn new(method: &'static str, table: Option<Table>, allowed: &[&str]) -> mlua::Result<Self> {
        if let Some(table) = &table {
            for pair in table.pairs::<Value, Value>() {
                let (key, _) = pair?;
                let known = key.as_string().is_some_and(|k| allowed.iter().any(|a| k == *a));
                if !known {
                    return Err(mlua::Error::runtime(format!(
                        "ui:{method}: unknown option {}; it takes {}",
                        key.to_string()?,
                        allowed.join(", ")
                    )));
                }
            }
        }
        Ok(Self { method, table })
    }

    fn get<T: mlua::FromLua>(&self, key: &str) -> mlua::Result<Option<T>> {
        match &self.table {
            Some(table) => table.get::<Option<T>>(key).map_err(|e| self.error(format!("{key}: {e}"))),
            None => Ok(None),
        }
    }

    fn optional_number(&self, key: &str) -> mlua::Result<Option<f32>> {
        self.get::<f32>(key)
    }

    fn number(&self, key: &str, default: f32) -> mlua::Result<f32> {
        Ok(self.optional_number(key)?.unwrap_or(default))
    }

    fn string(&self, key: &str) -> mlua::Result<Option<String>> {
        self.get::<String>(key)
    }

    /// `font`: a .ttf or .otf from `keyboard.resource(...)`.
    fn font(&self) -> mlua::Result<Option<Resource>> {
        match self.get::<AnyUserData>("font")? {
            None => Ok(None),
            Some(handle) => match resource_of(&handle)? {
                ResourceRef::Packaged(resource) => Ok(Some(resource)),
                live @ ResourceRef::Live(_) => Err(self.error(format!("font: {live} can't be a font; bundle it and use keyboard.resource(...)"))),
            },
        }
    }

    /// `w` and how text overflows it; ellipsis when `overflow` isn't given.
    fn fit(&self) -> mlua::Result<Option<Fit>> {
        let overflow = self.string("overflow")?;
        let Some(width) = self.optional_number("w")? else {
            return match overflow {
                Some(_) => Err(self.error("overflow needs w, the width the text overflows".into())),
                None => self.check_overflow_only("ellipsis").map(|()| None),
            };
        };
        let name = overflow.as_deref().unwrap_or("ellipsis");
        self.check_overflow_only(name)?;
        let overflow = match name {
            "ellipsis" => Overflow::Ellipsis,
            "shrink" => Overflow::Shrink { min_size: self.optional_number("min_size")? },
            "wrap" => Overflow::Wrap {
                lines: self.get::<u32>("lines")?,
                line_height: self.number("line_height", 1.2)?,
                height: self.optional_number("h")?,
            },
            "scroll" => Overflow::Scroll { speed: self.number("speed", 30.0)?, pause: self.number("pause", 1.5)? },
            "clip" => Overflow::Clip,
            other => return Err(self.error(format!("overflow {other:?} is not ellipsis, shrink, wrap, scroll or clip"))),
        };
        Ok(Some(Fit { width, overflow }))
    }

    fn check_overflow_only(&self, chosen: &str) -> mlua::Result<()> {
        for (key, mode) in OVERFLOW_ONLY {
            if mode != chosen && self.get::<Value>(key)?.is_some_and(|v| !v.is_nil()) {
                return Err(self.error(format!("{key} only works with overflow = \"{mode}\"")));
            }
        }
        Ok(())
    }

    fn color(&self, key: &str) -> mlua::Result<Option<Color>> {
        self.string(key)?.map(|text| Color::parse(&text)).transpose().map_err(self.wrap())
    }

    /// `x`, `y`, `w`, `h`, defaulting to the whole module.
    fn area(&self, (width, height): (f32, f32)) -> mlua::Result<Area> {
        Ok(Area { x: self.number("x", 0.0)?, y: self.number("y", 0.0)?, w: self.number("w", width)?, h: self.number("h", height)? })
    }

    fn error(&self, message: String) -> mlua::Error {
        mlua::Error::runtime(format!("ui:{}: {message}", self.method))
    }

    fn wrap(&self) -> impl Fn(anyhow::Error) -> mlua::Error + '_ {
        |e| self.error(format!("{e:#}"))
    }
}
