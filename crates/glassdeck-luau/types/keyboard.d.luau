-- keyboard.luau: runs on the keyboard, draws in the widget's own rectangle.
-- Callbacks are registered on the injected `widget` table, widget.on_draw(function(ui) ... end), and the
-- script keeps its state in its own locals.
-- Colors are "#rrggbb" or "#rrggbbaa". Coordinates are local: (0, 0) is the widget's top-left.

export type TextOptions = {
    x: number?,
    y: number?,
    size: number?,
    color: string?,
    -- "left", "center" or "right": within w when given, otherwise around x
    align: ("left" | "center" | "right")?,
    -- shrink the text to fit this width
    w: number?,
    -- a .ttf or .otf the module bundles, from keyboard.resource(...); the built-in font otherwise
    font: Resource?,
}

export type RectOptions = { fill: string?, stroke: string?, stroke_width: number?, radius: number? }

-- x, y, w and h default to the whole module
export type AreaOptions = { x: number?, y: number?, w: number?, h: number? }

export type BarOptions = { x: number?, y: number?, w: number?, h: number?, color: string? }

export type GraphOptions = { x: number?, y: number?, w: number?, h: number?, color: string?, min: number?, max: number? }

export type ArcOptions = { width: number?, color: string? }

declare extern type Ui with
    width: number
    height: number

    -- Drawing, only inside widget.on_draw
    function text(self, text: string, options: TextOptions?): ()
    function rect(self, x: number, y: number, w: number, h: number, options: RectOptions?): ()
    function bar(self, fraction: number, options: BarOptions?): ()
    function graph(self, values: { number }, options: GraphOptions?): ()
    -- an .svg or .png: ui:image(keyboard.resource("icons/dot.svg"), ...)
    function image(self, image: Resource, options: AreaOptions?): ()
    -- degrees, clockwise from 12 o'clock
    function arc(self, cx: number, cy: number, r: number, from: number, to: number, options: ArcOptions?): ()
end

-- Each callback is registered once; a second call is an error.
export type Widget = {
    -- Draws the widget, with the only Ui there is; required
    on_draw: (callback: (ui: Ui) -> ()) -> (),
    -- What the host half sent, or the options as they are with no host.luau; the widget redraws after
    on_data: (callback: (data: any) -> ()) -> (),
    -- The widget was resized; it redraws after
    on_resize: (callback: (w: number, h: number) -> ()) -> (),
    -- Before a hot reload; what it returns is widget.saved in the next run
    on_unload: (callback: () -> any) -> (),
    -- What on_unload returned before a hot reload; nil on a fresh start
    saved: any,
    every: (seconds: number, callback: () -> ()) -> (),
    redraw: () -> (),
    -- runs once at the next screen refresh, then on_draw runs; ask again to keep animating
    request_frame: (callback: (dt: number) -> ()) -> (),
}

declare widget: Widget

-- What the host half sent with live.put; draws nothing until it arrives. Names are one word.
declare live: { resource: (name: string) -> Resource }
