-- keyboard.luau in a scene: owns the scene's data and writes the shader's inputs.
-- Callbacks are registered with a call, scene.on_frame(function(frame, settings) ... end), so their parameters are typed.
-- `glassdeck pack` writes this beside inputs.d.luau, which declares `inputs` from the shader's [Inputs].
-- Vectors come from Luau's own vector.create(x, y, z?, w?).

-- sRGB from 0 to 1, as r, g, b, a in a vector
export type Color = vector

export type Frame = {
    -- seconds since the scene started
    time: number,
    -- seconds since the last frame
    delta: number,
    index: number,
}

-- The current value of each setting the scene declared
export type Settings = { [string]: any }

-- What setting.color, setting.range and setting.choice return, for scene.settings
declare extern type Setting with end

declare setting: {
    -- "#rgb", "#rrggbb", "#rrggbbaa", color.rgb(...) or color.hsv(...)
    color: (value: string | Color) -> Setting,
    range: (default: number, min: number, max: number) -> Setting,
    choice: (default: string, options: { string }) -> Setting,
}

declare color: {
    hex: (text: string) -> Color,
    -- 0 to 255 each; alpha defaults to 255
    rgb: (r: number, g: number, b: number, a: number?) -> Color,
    -- hue in degrees, saturation and value 0 to 1; alpha defaults to 1
    hsv: (h: number, s: number, v: number, a: number?) -> Color,
}

-- Wanders around centre by up to amount: x follows sin(t * speed.x + phase.x), y cos(t * speed.y + phase.y)
declare function drift(t: number, centre: vector, amount: vector, speed: vector, phase: vector?): vector

export type Scene = {
    -- A plain number, boolean or string is a setting of that kind with that default; label from the key
    settings: { [string]: number | boolean | string | Setting }?,
    -- Called once before the first draw, then whenever the user changes a setting
    on_settings: (callback: (settings: Settings) -> ()) -> (),
    -- Called every frame; a scene that never registers one does nothing per frame
    on_frame: (callback: (frame: Frame, settings: Settings) -> ()) -> (),
}

declare scene: Scene
