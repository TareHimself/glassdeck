//! The `ui` object `script.on_draw` gets: drawing calls, which only work while it runs. At the top
//! level every call draws where its x and y say; `ui:row`, `ui:column` and `ui:stack` lay their
//! children out instead, and `ui:box` gives a laid out box back to draw in by hand.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result};
use glassdeck_luau::{ResourceRef, resource_of};
use glassdeck_package::Resource;
use mlua::{AnyUserData, FromLua, Function, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};
use taffy::prelude::{AvailableSpace, Size};

use crate::draw::{Align, Area, Color, Command, Measure};
use crate::layout::{self, Arrange, Inset, Kind, Length, Node, Placement, Rect};
use crate::text::{self, Fit, Overflow};

/// A bar's thickness when nothing sizes it.
const BAR_HEIGHT: f32 = 8.0;
/// A graph's height when nothing sizes it.
const GRAPH_HEIGHT: f32 = 24.0;
/// The size text is measured at to find the size that fills a box.
const FILL_PROBE_PX: f32 = 100.0;

#[derive(Clone)]
pub struct Ui(Rc<RefCell<UiState>>);

struct UiState {
    width: u32,
    height: u32,
    /// Open while `draw` runs: the widget's scope, then one per `ui:box` being drawn. Drawing at any
    /// other time is an error.
    scopes: Option<Vec<Scope>>,
    redraw: bool,
    frames: Vec<Function>,
}

/// Where calls land: a widget or a box, with the containers open in it.
struct Scope {
    size: (f32, f32),
    items: Vec<Item>,
    open: Vec<Open>,
}

/// A container whose callback is still running.
struct Open {
    placement: Placement,
    arrange: Arrange,
    decor: Option<Decor>,
    children: Vec<UiNode>,
    /// Where a top-level container sits in its scope.
    origin: (f32, f32),
}

/// What a widget drew, in call order: drawing as it is, or a laid out tree.
pub enum Item {
    Draw(Command),
    Tree { origin: (f32, f32), space: (f32, f32), root: UiNode },
}

type UiNode = Node<Leaf, Option<Decor>>;

/// A container's own background.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decor {
    fill: Option<Color>,
    stroke: Option<(Color, f32)>,
    radius: f32,
}

/// Something a container lays out.
pub enum Leaf {
    Text(FlowText),
    Image { resource: ResourceRef, radius: f32 },
    Bar { fraction: f32, color: Color },
    Graph { values: Vec<f32>, color: Color, min: Option<f32>, max: Option<f32> },
    Rect(Decor),
    /// Drawn by hand in its laid out box, once the layout is known.
    Box(Function),
}

pub struct FlowText {
    text: String,
    size: TextSize,
    color: Color,
    align: Align,
    overflow: Overflow,
    font: Option<Resource>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TextSize {
    Px(f32),
    /// As big as fits the box.
    Fill,
}

/// The size `ui:box`'s callback gets.
pub struct BoxSize {
    width: f32,
    height: f32,
}

/// Runs a `ui:box` callback the way the VM runs any script call, with its time limit.
pub type CallBox<'a> = &'a dyn Fn(&Function, BoxSize) -> Result<()>;

impl UserData for BoxSize {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("width", |_, size| Ok(size.width));
        fields.add_field_method_get("height", |_, size| Ok(size.height));
    }
}

/// Whether a call draws where it says or joins the container being built.
enum Where {
    Top { size: (f32, f32) },
    Flow,
}

impl Ui {
    pub fn new(width: u32, height: u32) -> Self {
        Self(Rc::new(RefCell::new(UiState { width, height, scopes: None, redraw: true, frames: Vec::new() })))
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
        let mut state = self.0.borrow_mut();
        let size = (state.width as f32, state.height as f32);
        state.scopes = Some(vec![Scope { size, items: Vec::new(), open: Vec::new() }]);
    }

    /// Lays out what `on_draw` drew and turns it into drawing, calling each `ui:box`'s callback with
    /// its box.
    pub fn finish_draw(&self, measure: &mut dyn Measure, call: CallBox) -> Result<Vec<Command>> {
        let items = self.scopes_mut(|scopes| scopes.first_mut().map(|scope| std::mem::take(&mut scope.items)))?.unwrap_or_default();
        self.expand(items, (0.0, 0.0), measure, call)
    }

    pub fn end_draw(&self) {
        self.0.borrow_mut().scopes = None;
    }

    /// `at` moves it all, for a box's own drawing.
    fn expand(&self, items: Vec<Item>, at: (f32, f32), measure: &mut dyn Measure, call: CallBox) -> Result<Vec<Command>> {
        let mut out = Vec::new();
        for item in items {
            match item {
                Item::Draw(command) => out.push(command.translated(at)),
                Item::Tree { origin, space, root } => {
                    let failed = RefCell::new(None);
                    let rects = layout::lay_out(&root, space, |leaf, known, available| {
                        measure_leaf(leaf, known, available, measure).unwrap_or_else(|e| {
                            failed.borrow_mut().get_or_insert(e);
                            (0.0, 0.0)
                        })
                    })?;
                    if let Some(e) = failed.into_inner() {
                        return Err(e.context("measuring the layout"));
                    }
                    let origin = (at.0 + origin.0, at.1 + origin.1);
                    let mut rects = rects.into_iter();
                    self.emit(&root, &mut rects, origin, measure, call, &mut out)?;
                }
            }
        }
        Ok(out)
    }

    fn emit(
        &self,
        node: &UiNode,
        rects: &mut impl Iterator<Item = Rect>,
        origin: (f32, f32),
        measure: &mut dyn Measure,
        call: CallBox,
        out: &mut Vec<Command>,
    ) -> Result<()> {
        let laid = rects.next().context("the layout has fewer boxes than the tree")?;
        let r = Rect { x: laid.x + origin.0, y: laid.y + origin.1, ..laid };
        let area = Area { x: r.x, y: r.y, w: r.w, h: r.h };
        match node {
            Node::Container { decor, children, .. } => {
                if let Some(decor) = decor {
                    out.push(Command::Rect { area, fill: decor.fill, stroke: decor.stroke, radius: decor.radius });
                }
                for child in children {
                    self.emit(child, rects, origin, measure, call, out)?;
                }
            }
            Node::Leaf { leaf, .. } => match leaf {
                Leaf::Text(text) => out.push(text_in(text, r, measure)?),
                Leaf::Image { resource, radius } => out.push(Command::Image { resource: resource.clone(), area, radius: *radius }),
                Leaf::Bar { fraction, color } => out.push(Command::Bar { fraction: *fraction, area, color: *color }),
                Leaf::Graph { values, color, min, max } => {
                    out.push(Command::Graph { values: values.clone(), area, color: *color, min: *min, max: *max })
                }
                Leaf::Rect(decor) => out.push(Command::Rect { area, fill: decor.fill, stroke: decor.stroke, radius: decor.radius }),
                Leaf::Box(draw) => {
                    let items = self.in_box(draw, (r.w, r.h), call).context("in ui:box")?;
                    out.extend(self.expand(items, (r.x, r.y), measure, call)?);
                }
            },
        }
        Ok(())
    }

    /// Runs a box's callback in a scope of its own, so it draws in the box's coordinates.
    fn in_box(&self, draw: &Function, size: (f32, f32), call: CallBox) -> Result<Vec<Item>> {
        self.scopes_mut(|scopes| scopes.push(Scope { size, items: Vec::new(), open: Vec::new() }))?;
        let result = call(draw, BoxSize { width: size.0, height: size.1 });
        let scope = self.scopes_mut(|scopes| scopes.pop())?.context("the box's scope is gone")?;
        result?;
        Ok(scope.items)
    }

    fn scopes_mut<T>(&self, change: impl FnOnce(&mut Vec<Scope>) -> T) -> Result<T> {
        let mut state = self.0.borrow_mut();
        let scopes = state.scopes.as_mut().context("drawing has finished")?;
        Ok(change(scopes))
    }

    fn place(&self, method: &str) -> mlua::Result<Where> {
        let state = self.0.borrow();
        let scope = state.scopes.as_ref().and_then(|scopes| scopes.last()).ok_or_else(|| outside(method))?;
        Ok(if scope.open.is_empty() { Where::Top { size: scope.size } } else { Where::Flow })
    }

    fn record(&self, method: &str, command: Command) -> mlua::Result<()> {
        let mut state = self.0.borrow_mut();
        let scope = state.scopes.as_mut().and_then(|scopes| scopes.last_mut()).ok_or_else(|| outside(method))?;
        scope.items.push(Item::Draw(command));
        Ok(())
    }

    fn add_leaf(&self, method: &str, node: UiNode) -> mlua::Result<()> {
        let mut state = self.0.borrow_mut();
        let scope = state.scopes.as_mut().and_then(|scopes| scopes.last_mut()).ok_or_else(|| outside(method))?;
        let open = scope.open.last_mut().ok_or_else(|| mlua::Error::runtime(format!("ui:{method}: only inside a row, column or stack")))?;
        open.children.push(node);
        Ok(())
    }

    /// Runs a container's callback with the container open, then files it with its parent.
    fn container(&self, method: &'static str, kind: Kind, opts: Option<Table>, build: Function) -> mlua::Result<()> {
        let context = self.place(method)?;
        let o = Opts::new(method, opts, &container_options(&context))?;
        let (placement, origin) = match context {
            Where::Top { size } => {
                let (x, y) = (o.number("x", 0.0)?, o.number("y", 0.0)?);
                let w = o.length("w")?.unwrap_or(Length::Px(size.0 - x));
                let h = o.length("h")?.unwrap_or(Length::Px(size.1 - y));
                (Placement { w, h, ..Placement::default() }, (x, y))
            }
            Where::Flow => (o.placement()?, (0.0, 0.0)),
        };
        let arrange = Arrange {
            kind,
            gap: o.number("gap", 0.0)?,
            padding: o.padding()?,
            align: o.string("align")?.map(|a| layout::Align::parse(&a)).transpose().map_err(o.wrap())?,
            justify: o.string("justify")?.map(|j| layout::Justify::parse(&j)).transpose().map_err(o.wrap())?,
        };
        let decor = o.decor(false)?;
        self.scopes_mut(|scopes| {
            let scope = scopes.last_mut().expect("context found a scope");
            scope.open.push(Open { placement, arrange, decor, children: Vec::new(), origin });
        })
        .map_err(|_| outside(method))?;
        let built = build.call::<()>(());
        let mut state = self.0.borrow_mut();
        let scope = state.scopes.as_mut().and_then(|scopes| scopes.last_mut()).ok_or_else(|| outside(method))?;
        let open = scope.open.pop().ok_or_else(|| mlua::Error::runtime(format!("ui:{method}: its container is gone")))?;
        built?;
        let node = Node::Container { placement: open.placement, arrange: open.arrange, decor: open.decor, children: open.children };
        match scope.open.last_mut() {
            Some(parent) => parent.children.push(node),
            None => scope.items.push(Item::Tree { origin: open.origin, space: scope.size, root: node }),
        }
        Ok(())
    }

    /// `ui.width` and `ui.height`: the box being drawn in, else the widget.
    fn size(&self) -> (f32, f32) {
        let state = self.0.borrow();
        state
            .scopes
            .as_ref()
            .and_then(|scopes| scopes.last())
            .map_or((state.width as f32, state.height as f32), |scope| scope.size)
    }
}

fn outside(method: &str) -> mlua::Error {
    mlua::Error::runtime(format!("ui:{method} only works inside script.on_draw"))
}

/// A leaf's own size, for the layout: `known` sides are already decided by the container.
fn measure_leaf(leaf: &Leaf, known: Size<Option<f32>>, available: Size<AvailableSpace>, measure: &mut dyn Measure) -> Result<(f32, f32)> {
    Ok(match leaf {
        Leaf::Text(FlowText { size: TextSize::Fill, .. }) => (known.width.unwrap_or(0.0), known.height.unwrap_or(0.0)),
        Leaf::Text(text @ FlowText { size: TextSize::Px(px), .. }) => {
            let font = text.font.as_ref();
            let line = measure.line_height(font, *px)?;
            let natural = measure.text_width(font, &text.text, *px)?;
            match text.overflow {
                Overflow::Wrap { lines, line_height, .. } => {
                    let width = known.width.or(match available.width {
                        AvailableSpace::Definite(w) => Some(w),
                        _ => None,
                    });
                    match width {
                        Some(width) if natural > width => {
                            let fit = Fit { width, overflow: Overflow::Wrap { lines, line_height, height: known.height } };
                            let measure = RefCell::new(measure);
                            let laid = text::lay_out(&text.text, *px, text.align, Some(fit), &|s, p| {
                                measure.borrow_mut().text_width(font, s, p).unwrap_or(0.0)
                            });
                            let rows = laid.lines.len().max(1) as f32;
                            (width, (rows - 1.0) * px * line_height + line)
                        }
                        _ => (natural, line),
                    }
                }
                _ if available.width == AvailableSpace::MinContent => (0.0, line),
                _ => (natural, line),
            }
        }
        Leaf::Image { resource, .. } => match measure.image_size(resource)? {
            None => (0.0, 0.0),
            Some((w, h)) => match (known.width, known.height) {
                (Some(kw), None) if w > 0.0 => (kw, kw * h / w),
                (None, Some(kh)) if h > 0.0 => (kh * w / h, kh),
                _ => (w, h),
            },
        },
        Leaf::Bar { .. } => (0.0, BAR_HEIGHT),
        Leaf::Graph { .. } => (0.0, GRAPH_HEIGHT),
        Leaf::Rect(_) | Leaf::Box(_) => (0.0, 0.0),
    })
}

/// Text placed in its laid out box: centred down it, fitted across it, and as big as fits for `"fill"`.
fn text_in(text: &FlowText, r: Rect, measure: &mut dyn Measure) -> Result<Command> {
    let font = text.font.as_ref();
    let px = match text.size {
        TextSize::Px(px) => px,
        TextSize::Fill => {
            let line = measure.line_height(font, FILL_PROBE_PX)?;
            let width = measure.text_width(font, &text.text, FILL_PROBE_PX)?;
            let by_height = if line > 0.0 { r.h * FILL_PROBE_PX / line } else { r.h };
            let by_width = if width > 0.0 { r.w * FILL_PROBE_PX / width } else { by_height };
            by_height.min(by_width).max(1.0)
        }
    };
    let (y, overflow) = match text.overflow {
        Overflow::Wrap { lines, line_height, .. } => (r.y, Overflow::Wrap { lines, line_height, height: Some(r.h) }),
        other => (r.y + (r.h - measure.line_height(font, px)?) / 2.0, other),
    };
    Ok(Command::Text {
        text: text.text.clone(),
        x: r.x,
        y,
        size: px,
        color: text.color,
        align: text.align,
        fit: Some(Fit { width: r.w, overflow }),
        font: text.font.clone(),
    })
}

impl UserData for Ui {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("width", |_, ui| Ok(ui.size().0));
        fields.add_field_method_get("height", |_, ui| Ok(ui.size().1));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("text", |_, ui, (text, opts): (String, Option<Table>)| match ui.place("text")? {
            Where::Top { .. } => {
                let o = Opts::new("text", opts, TEXT_OPTIONS)?;
                let command = Command::Text {
                    text,
                    x: o.number("x", 0.0)?,
                    y: o.number("y", 0.0)?,
                    size: o.number("size", 16.0).map_err(|_| o.error("size \"fill\" only works inside a row, column or stack".into()))?,
                    color: o.color("color")?.unwrap_or(Color::WHITE),
                    align: o.align()?,
                    fit: o.fit()?,
                    font: o.font()?,
                };
                ui.record("text", command)
            }
            Where::Flow => {
                let o = Opts::new("text", opts, &with_placement(FLOW_TEXT_OPTIONS))?;
                let size = match o.get::<Value>("size")? {
                    Some(Value::String(s)) if s.to_str()?.as_ref() == "fill" => TextSize::Fill,
                    Some(Value::String(s)) => return Err(o.error(format!("size {:?} is not a number or \"fill\"", s.to_str()?.as_ref()))),
                    Some(_) => TextSize::Px(o.number("size", 16.0)?),
                    None => TextSize::Px(16.0),
                };
                let mut placement = o.placement()?;
                placement.shrink = 1.0;
                if size == TextSize::Fill && placement.grow == 0.0 {
                    placement.grow = 1.0;
                }
                let leaf = FlowText {
                    text,
                    size,
                    color: o.color("color")?.unwrap_or(Color::WHITE),
                    align: o.align()?,
                    overflow: o.flow_overflow(size)?,
                    font: o.font()?,
                };
                ui.add_leaf("text", Node::Leaf { placement, leaf: Leaf::Text(leaf) })
            }
        });
        methods.add_method("rect", |lua, ui, args: MultiValue| match ui.place("rect")? {
            Where::Top { .. } => {
                let mut args = args.into_iter();
                let mut number = |name: &str| -> mlua::Result<f32> {
                    f32::from_lua(args.next().unwrap_or(Value::Nil), lua).map_err(|_| mlua::Error::runtime(format!("ui:rect: {name} must be a number")))
                };
                let (x, y, w, h) = (number("x")?, number("y")?, number("w")?, number("h")?);
                let opts = Option::<Table>::from_lua(args.next().unwrap_or(Value::Nil), lua)?;
                let o = Opts::new("rect", opts, RECT_OPTIONS)?;
                let decor = o.decor(true)?.expect("a rect always has a look");
                ui.record("rect", Command::Rect { area: Area { x, y, w, h }, fill: decor.fill, stroke: decor.stroke, radius: decor.radius })
            }
            Where::Flow => {
                let mut args = args.into_iter();
                let opts = Option::<Table>::from_lua(args.next().unwrap_or(Value::Nil), lua)
                    .map_err(|_| mlua::Error::runtime("ui:rect: inside a row, column or stack it takes only an options table"))?;
                let o = Opts::new("rect", opts, &with_placement(RECT_OPTIONS))?;
                let decor = o.decor(true)?.expect("a rect always has a look");
                ui.add_leaf("rect", Node::Leaf { placement: o.placement()?, leaf: Leaf::Rect(decor) })
            }
        });
        methods.add_method("bar", |_, ui, (fraction, opts): (f32, Option<Table>)| match ui.place("bar")? {
            Where::Top { size } => {
                let o = Opts::new("bar", opts, &["x", "y", "w", "h", "color"])?;
                ui.record("bar", Command::Bar { fraction, area: o.area(size)?, color: o.color("color")?.unwrap_or(Color::WHITE) })
            }
            Where::Flow => {
                let o = Opts::new("bar", opts, &with_placement(&["color"]))?;
                let leaf = Leaf::Bar { fraction, color: o.color("color")?.unwrap_or(Color::WHITE) };
                ui.add_leaf("bar", Node::Leaf { placement: o.placement()?, leaf })
            }
        });
        methods.add_method("graph", |_, ui, (values, opts): (Vec<f32>, Option<Table>)| match ui.place("graph")? {
            Where::Top { size } => {
                let o = Opts::new("graph", opts, &["x", "y", "w", "h", "color", "min", "max"])?;
                let command = Command::Graph {
                    values,
                    area: o.area(size)?,
                    color: o.color("color")?.unwrap_or(Color::WHITE),
                    min: o.optional_number("min")?,
                    max: o.optional_number("max")?,
                };
                ui.record("graph", command)
            }
            Where::Flow => {
                let o = Opts::new("graph", opts, &with_placement(&["color", "min", "max"]))?;
                let leaf = Leaf::Graph {
                    values,
                    color: o.color("color")?.unwrap_or(Color::WHITE),
                    min: o.optional_number("min")?,
                    max: o.optional_number("max")?,
                };
                ui.add_leaf("graph", Node::Leaf { placement: o.placement()?, leaf })
            }
        });
        methods.add_method("image", |_, ui, (handle, opts): (AnyUserData, Option<Table>)| match ui.place("image")? {
            Where::Top { size } => {
                let o = Opts::new("image", opts, &["x", "y", "w", "h", "radius"])?;
                ui.record("image", Command::Image { resource: resource_of(&handle)?, area: o.area(size)?, radius: o.number("radius", 0.0)? })
            }
            Where::Flow => {
                let o = Opts::new("image", opts, &with_placement(&["radius"]))?;
                let leaf = Leaf::Image { resource: resource_of(&handle)?, radius: o.number("radius", 0.0)? };
                ui.add_leaf("image", Node::Leaf { placement: o.placement()?, leaf })
            }
        });
        methods.add_method("arc", |_, ui, (cx, cy, r, from, to, opts): (f32, f32, f32, f32, f32, Option<Table>)| match ui.place("arc")? {
            Where::Top { .. } => {
                let o = Opts::new("arc", opts, &["width", "color"])?;
                let command =
                    Command::Arc { cx, cy, r, from, to, width: o.number("width", 6.0)?, color: o.color("color")?.unwrap_or(Color::WHITE) };
                ui.record("arc", command)
            }
            Where::Flow => Err(mlua::Error::runtime("ui:arc needs a box to draw in: put it in ui:box(options, function(box) ... end)")),
        });
        methods.add_method("row", |_, ui, (opts, build): (Option<Table>, Function)| ui.container("row", Kind::Row, opts, build));
        methods.add_method("column", |_, ui, (opts, build): (Option<Table>, Function)| ui.container("column", Kind::Column, opts, build));
        methods.add_method("stack", |_, ui, (opts, build): (Option<Table>, Function)| ui.container("stack", Kind::Stack, opts, build));
        methods.add_method("box", |_, ui, (opts, draw): (Option<Table>, Function)| match ui.place("box")? {
            Where::Top { .. } => Err(mlua::Error::runtime("ui:box only works inside a row, column or stack; at the top level draw where you like")),
            Where::Flow => {
                let o = Opts::new("box", opts, PLACEMENT_OPTIONS)?;
                ui.add_leaf("box", Node::Leaf { placement: o.placement()?, leaf: Leaf::Box(draw) })
            }
        });
    }
}

const TEXT_OPTIONS: &[&str] =
    &["x", "y", "size", "color", "align", "w", "font", "overflow", "min_size", "lines", "line_height", "h", "speed", "pause"];
const FLOW_TEXT_OPTIONS: &[&str] = &["size", "color", "align", "font", "overflow", "min_size", "lines", "line_height", "speed", "pause"];
const RECT_OPTIONS: &[&str] = &["fill", "stroke", "stroke_width", "radius"];
/// How anything sits in a row, column or stack.
const PLACEMENT_OPTIONS: &[&str] = &["w", "h", "grow", "position", "x", "y", "right", "bottom"];
const ARRANGE_OPTIONS: &[&str] = &["gap", "padding", "align", "justify", "fill", "stroke", "stroke_width", "radius"];

fn with_placement(own: &[&'static str]) -> Vec<&'static str> {
    own.iter().chain(PLACEMENT_OPTIONS).copied().collect()
}

fn container_options(context: &Where) -> Vec<&'static str> {
    match context {
        Where::Top { .. } => ARRANGE_OPTIONS.iter().chain(&["x", "y", "w", "h"]).copied().collect(),
        Where::Flow => with_placement(ARRANGE_OPTIONS),
    }
}

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

    fn align(&self) -> mlua::Result<Align> {
        Ok(self.string("align")?.map(|a| Align::parse(&a)).transpose().map_err(self.wrap())?.unwrap_or(Align::Left))
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
                None => self.check_overflow_only("ellipsis", true).map(|()| None),
            };
        };
        let name = overflow.as_deref().unwrap_or("ellipsis");
        self.check_overflow_only(name, true)?;
        Ok(Some(Fit { width, overflow: self.overflow(name, self.optional_number("h")?)? }))
    }

    /// How text in a row, column or stack overflows its laid out box; its width is the box's.
    fn flow_overflow(&self, size: TextSize) -> mlua::Result<Overflow> {
        let name = self.string("overflow")?.unwrap_or_else(|| "ellipsis".into());
        self.check_overflow_only(&name, false)?;
        if size == TextSize::Fill && name == "wrap" {
            return Err(self.error("size \"fill\" can't wrap: it already picks the size that fits on one line".into()));
        }
        self.overflow(&name, None)
    }

    fn overflow(&self, name: &str, height: Option<f32>) -> mlua::Result<Overflow> {
        Ok(match name {
            "ellipsis" => Overflow::Ellipsis,
            "shrink" => Overflow::Shrink { min_size: self.optional_number("min_size")? },
            "wrap" => Overflow::Wrap { lines: self.get::<u32>("lines")?, line_height: self.number("line_height", 1.2)?, height },
            "scroll" => Overflow::Scroll { speed: self.number("speed", 30.0)?, pause: self.number("pause", 1.5)? },
            "clip" => Overflow::Clip,
            other => return Err(self.error(format!("overflow {other:?} is not ellipsis, shrink, wrap, scroll or clip"))),
        })
    }

    /// At the top level `h` is how tall wrapped text may get; in a layout it's the box's height.
    fn check_overflow_only(&self, chosen: &str, top: bool) -> mlua::Result<()> {
        for (key, mode) in OVERFLOW_ONLY {
            if (top || key != "h") && mode != chosen && self.get::<Value>(key)?.is_some_and(|v| !v.is_nil()) {
                return Err(self.error(format!("{key} only works with overflow = \"{mode}\"")));
            }
        }
        Ok(())
    }

    fn color(&self, key: &str) -> mlua::Result<Option<Color>> {
        self.string(key)?.map(|text| Color::parse(&text)).transpose().map_err(self.wrap())
    }

    /// `x`, `y`, `w`, `h`, defaulting to the whole of `size`.
    fn area(&self, (width, height): (f32, f32)) -> mlua::Result<Area> {
        Ok(Area { x: self.number("x", 0.0)?, y: self.number("y", 0.0)?, w: self.number("w", width)?, h: self.number("h", height)? })
    }

    /// A number of pixels, `"50%"` or `"fit"`.
    fn length(&self, key: &str) -> mlua::Result<Option<Length>> {
        match self.get::<Value>(key)? {
            None | Some(Value::Nil) => Ok(None),
            Some(Value::String(text)) => Length::parse_text(&text.to_str()?).map(Some).map_err(|e| self.error(format!("{key}: {e:#}"))),
            Some(_) => Ok(Some(Length::Px(self.number(key, 0.0)?))),
        }
    }

    /// How the element sits in its row, column or stack.
    fn placement(&self) -> mlua::Result<Placement> {
        let absolute = match self.string("position")?.as_deref() {
            None => {
                if let Some(key) = ["x", "y", "right", "bottom"].into_iter().find(|key| self.get::<Value>(key).is_ok_and(|v| v.is_some())) {
                    return Err(self.error(format!("{key} only works with position = \"absolute\" inside a row, column or stack")));
                }
                None
            }
            Some("absolute") => Some(Inset {
                left: self.optional_number("x")?,
                top: self.optional_number("y")?,
                right: self.optional_number("right")?,
                bottom: self.optional_number("bottom")?,
            }),
            Some(other) => return Err(self.error(format!("position {other:?} is not \"absolute\""))),
        };
        Ok(Placement {
            w: self.length("w")?.unwrap_or(Length::Fit),
            h: self.length("h")?.unwrap_or(Length::Fit),
            grow: self.number("grow", 0.0)?,
            absolute,
            ..Placement::default()
        })
    }

    /// `padding`: one number for every side, or `{ top, right, bottom, left }`.
    fn padding(&self) -> mlua::Result<[f32; 4]> {
        match self.get::<Value>("padding")? {
            None | Some(Value::Nil) => Ok([0.0; 4]),
            Some(Value::Table(sides)) => {
                let side = |name: &str| -> mlua::Result<f32> {
                    Ok(sides.get::<Option<f32>>(name).map_err(|e| self.error(format!("padding.{name}: {e}")))?.unwrap_or(0.0))
                };
                for pair in sides.pairs::<Value, Value>() {
                    let (key, _) = pair?;
                    if !key.as_string().is_some_and(|k| ["top", "right", "bottom", "left"].iter().any(|s| k == *s)) {
                        return Err(self.error(format!("padding: unknown side {}; it takes top, right, bottom, left", key.to_string()?)));
                    }
                }
                Ok([side("top")?, side("right")?, side("bottom")?, side("left")?])
            }
            Some(_) => Ok([self.number("padding", 0.0)?; 4]),
        }
    }

    /// A rect's fill and stroke (white when it has neither), or a container's background if it has one.
    fn decor(&self, always: bool) -> mlua::Result<Option<Decor>> {
        let (fill, stroke) = (self.color("fill")?, self.color("stroke")?);
        let stroke = stroke.map(|color| self.number("stroke_width", 2.0).map(|width| (color, width))).transpose()?;
        if fill.is_none() && stroke.is_none() && !always {
            return Ok(None);
        }
        let fill = if fill.is_none() && stroke.is_none() { Some(Color::WHITE) } else { fill };
        Ok(Some(Decor { fill, stroke, radius: self.number("radius", 0.0)? }))
    }

    fn error(&self, message: String) -> mlua::Error {
        mlua::Error::runtime(format!("ui:{}: {message}", self.method))
    }

    fn wrap(&self) -> impl Fn(anyhow::Error) -> mlua::Error + '_ {
        |e| self.error(format!("{e:#}"))
    }
}

#[cfg(test)]
mod tests {
    use mlua::Lua;

    use super::*;

    /// Every glyph half as wide as the text is tall, and lines exactly as tall.
    struct Fixed;

    impl Measure for Fixed {
        fn text_width(&mut self, _: Option<&Resource>, text: &str, px: f32) -> Result<f32> {
            Ok(text.chars().count() as f32 * px / 2.0)
        }
        fn line_height(&mut self, _: Option<&Resource>, px: f32) -> Result<f32> {
            Ok(px)
        }
        fn image_size(&mut self, _: &ResourceRef) -> Result<Option<(f32, f32)>> {
            Ok(None)
        }
    }

    fn draw(on_draw: &str) -> Result<Vec<Command>> {
        let lua = Lua::new();
        let ui = Ui::new(100, 40);
        let draw: Function = lua.load(format!("return function(ui) {on_draw} end")).eval().map_err(|e| anyhow::anyhow!("{e}"))?;
        ui.begin_draw();
        let call: CallBox = &|f, size| f.call::<()>(size).map_err(|e| anyhow::anyhow!("{e}"));
        let result = draw.call::<()>(ui.clone()).map_err(|e| anyhow::anyhow!("{e}")).and_then(|()| ui.finish_draw(&mut Fixed, call));
        ui.end_draw();
        result
    }

    fn areas(commands: &[Command]) -> Vec<(f32, f32, f32, f32)> {
        commands
            .iter()
            .map(|c| match c {
                Command::Rect { area, .. } | Command::Bar { area, .. } | Command::Image { area, .. } | Command::Graph { area, .. } => {
                    (area.x, area.y, area.w, area.h)
                }
                Command::Text { x, y, size, fit, .. } => (*x, *y, fit.map_or(0.0, |f| f.width), *size),
                Command::Arc { cx, cy, r, .. } => (*cx, *cy, *r, *r),
            })
            .collect()
    }

    fn error(on_draw: &str) -> String {
        format!("{:#}", draw(on_draw).expect_err("it should fail"))
    }

    #[test]
    fn top_level_drawing_is_where_it_says() {
        let commands = draw("ui:rect(1, 2, 3, 4, { fill = '#ff0000' }); ui:text('hi', { x = 5, y = 6 })").unwrap();
        assert_eq!(areas(&commands), [(1.0, 2.0, 3.0, 4.0), (5.0, 6.0, 0.0, 16.0)]);
    }

    #[test]
    fn a_row_places_its_children_in_turn() {
        let commands = draw(
            "ui:row({ x = 10, padding = 2, gap = 4, align = 'start' }, function()
                ui:rect({ w = 10, h = 5 })
                ui:rect({ w = '25%', h = 5 })
                ui:rect({ grow = 1, h = 5 })
            end)",
        )
        .unwrap();
        assert_eq!(areas(&commands), [(12.0, 2.0, 10.0, 5.0), (26.0, 2.0, 22.0, 5.0), (52.0, 2.0, 46.0, 5.0)]);
    }

    #[test]
    fn a_stack_layers_its_children_over_its_background_in_call_order() {
        let commands = draw(
            "ui:stack({ fill = '#000000', radius = 4 }, function()
                ui:rect({ grow = 1 })
                ui:rect({ position = 'absolute', right = 2, bottom = 3, w = 6, h = 6 })
            end)",
        )
        .unwrap();
        assert_eq!(areas(&commands), [(0.0, 0.0, 100.0, 40.0), (0.0, 0.0, 100.0, 40.0), (92.0, 31.0, 6.0, 6.0)]);
        assert!(matches!(commands[0], Command::Rect { radius: 4.0, .. }));
    }

    #[test]
    fn a_box_draws_in_its_own_coordinates() {
        let commands = draw(
            "ui:column({ padding = 5 }, function()
                ui:box({ w = 20, h = 10 }, function(box) ui:rect(0, 0, box.width, box.height); ui:rect(1, 1, ui.width - 2, 2) end)
            end)",
        )
        .unwrap();
        assert_eq!(areas(&commands), [(5.0, 5.0, 20.0, 10.0), (6.0, 6.0, 18.0, 2.0)]);
    }

    #[test]
    fn fill_text_takes_the_largest_size_that_fits() {
        let tall = draw("ui:column({}, function() ui:text('abcd', { size = 'fill' }) end)").unwrap();
        assert!(matches!(tall[0], Command::Text { size, .. } if size == 40.0));
        let wide = draw("ui:column({}, function() ui:text('abcdefghij', { size = 'fill' }) end)").unwrap();
        assert!(matches!(wide[0], Command::Text { size, .. } if size == 20.0));
    }

    #[test]
    fn text_in_a_row_fits_its_measured_width_and_centres_down_it() {
        let commands = draw("ui:row({ h = 20, align = 'stretch' }, function() ui:text('abc', { size = 10 }) end)").unwrap();
        assert!(matches!(&commands[0], Command::Text { x: 0.0, y: 5.0, fit: Some(Fit { width: 15.0, .. }), .. }));
    }

    #[test]
    fn layout_mistakes_fail_loudly() {
        assert!(error("ui:box({}, function() end)").contains("only works inside a row, column or stack"));
        assert!(error("ui:row({}, function() ui:text('hi', { x = 3 }) end)").contains("x only works with position = \"absolute\""));
        assert!(error("ui:row({ justify = 'middle' }, function() end)").contains("middle"));
        assert!(error("ui:row({ w = 'half' }, function() end)").contains("w:"));
        assert!(error("ui:text('hi', { size = 'fill' })").contains("only works inside a row, column or stack"));
        assert!(error("ui:column({}, function() ui:arc(1, 1, 1, 0, 1) end)").contains("ui:box"));
        assert!(error("ui:row({ grow = 1 }, function() end)").contains("unknown option grow"));
    }
}
