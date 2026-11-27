-- keyboard.luau in a canvas: holds the canvas's state and writes the shader's inputs. What the user can
-- set lives in options.luau; the host half sends what the canvas needs over bridge.
-- Callbacks are registered with a call, script.on_frame(function(frame) ... end), so their parameters are typed.
-- `glassdeck pack` writes this beside inputs.d.luau, which declares `inputs` from the shader's [Inputs].
-- Vectors come from Luau's own vector.create(x, y, z?, w?).

-- sRGB from 0 to 1, as r, g, b, a in a vector
export type Color = vector

export type Frame = {
    -- seconds since the canvas started
    time: number,
    -- seconds since the last frame
    delta: number,
    index: number,
}

declare color: {
    -- "#rgb", "#rrggbb" or "#rrggbbaa"
    hex: (text: string) -> Color,
    -- 0 to 255 each; alpha defaults to 255
    rgb: (r: number, g: number, b: number, a: number?) -> Color,
    -- hue in degrees, saturation and value 0 to 1; alpha defaults to 1
    hsv: (h: number, s: number, v: number, a: number?) -> Color,
}

-- Wanders around centre by up to amount: x follows sin(t * speed.x + phase.x), y cos(t * speed.y + phase.y)
declare function drift(t: number, centre: vector, amount: vector, speed: vector, phase: vector?): vector

export type Canvas = {
    -- Called every frame; a canvas that never registers one does nothing per frame
    on_frame: (callback: (frame: Frame) -> ()) -> (),
}

-- This script: its callbacks
declare script: Canvas

-- Messages from the host half (bridge.luau): bridge.on.<message>(callback)
declare bridge: { on: MessagesHeard }

-- The host half: host.bridge.<message>(value) sends it one (bridge.luau)
declare host: { bridge: MessagesSent }
