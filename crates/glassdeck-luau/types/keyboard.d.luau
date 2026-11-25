-- keyboard.luau: runs on the keyboard, draws in the module's own rectangle.
-- Colors are "#rrggbb" or "#rrggbbaa". Coordinates are local: (0, 0) is the module's top-left.

type TextOptions = {
    x: number?,
    y: number?,
    size: number?,
    color: string?,
    -- "left", "center" or "right": within w when given, otherwise around x
    align: ("left" | "center" | "right")?,
    -- shrink the text to fit this width
    w: number?,
}

type RectOptions = { fill: string?, stroke: string?, stroke_width: number?, radius: number? }

-- x, y, w and h default to the whole module
type AreaOptions = { x: number?, y: number?, w: number?, h: number? }

type BarOptions = { x: number?, y: number?, w: number?, h: number?, color: string? }

type GraphOptions = { x: number?, y: number?, w: number?, h: number?, color: string?, min: number?, max: number? }

type ArcOptions = { width: number?, color: string? }

declare class Ui
    width: number
    height: number

    -- Drawing, only inside draw
    function text(self, text: string, options: TextOptions?): ()
    function rect(self, x: number, y: number, w: number, h: number, options: RectOptions?): ()
    function bar(self, fraction: number, options: BarOptions?): ()
    function graph(self, values: { number }, options: GraphOptions?): ()
    -- an .svg or .png: ui:image(keyboard.resource("icons/dot.svg"), ...)
    function image(self, image: Resource, options: AreaOptions?): ()
    -- degrees, clockwise from 12 o'clock
    function arc(self, cx: number, cy: number, r: number, from: number, to: number, options: ArcOptions?): ()

    -- Redraw triggers, usable anywhere
    function every(self, seconds: number, callback: () -> ()): ()
    function redraw(self): ()
    -- runs once at the next screen refresh, then draw runs; ask again to keep animating
    function request_frame(self, callback: (dt: number) -> ()): ()
end

declare ui: Ui

-- What keyboard.luau returns. Every hook is optional except draw.
type Module<State> = {
    init: ((settings: Settings, saved: Data?) -> State)?,
    -- the return value goes to the next init after a hot reload
    unload: ((state: State) -> Data?)?,
    resize: ((state: State, w: number, h: number) -> ())?,
    settings_changed: ((state: State, settings: Settings) -> ())?,
    -- return true to redraw
    on_data: ((state: State, data: Data) -> boolean)?,
    draw: (state: State, ui: Ui) -> (),
}
