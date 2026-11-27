type Data = { title: string?, artist: string?, playing: boolean?, position: number?, duration: number? }

local cover = live.resource("cover")

local title, artist, playing = "", "", false
local progress: number? = nil

widget.on_data(function(data: Data)
    title, artist, playing = data.title or "", data.artist or "", data.playing == true
    progress = if data.position and data.duration then data.position / data.duration else nil
end)

-- Text shrinks to fit its width, so a long title is cut short instead of shrinking to nothing.
local AVERAGE_GLYPH_WIDTH = 0.58

local function shorten(text: string, size: number, w: number): string
    local fits = math.floor(w / (size * AVERAGE_GLYPH_WIDTH))
    if utf8.len(text) <= fits then
        return text
    end
    return string.sub(text, 1, utf8.offset(text, fits) - 1) .. "…"
end

widget.on_draw(function(ui: Ui)
    local h = ui.height
    local pad = h * 0.13
    ui:rect(0, 0, ui.width, h, { fill = "#1d1838c0", stroke = "#ffffff12", stroke_width = 2, radius = h * 0.11 })

    local side = h - pad * 2
    ui:rect(pad, pad, side, side, { fill = "#ffffff14", radius = side * 0.1 })
    ui:image(cover, { x = pad, y = pad, w = side, h = side, radius = side * 0.1 })

    local x = pad + side + pad * 1.1
    local w = ui.width - x - pad
    if title == "" then
        ui:text("Nothing playing", { x = x, y = h * 0.42, size = h * 0.12, w = w, color = "#ffffff80" })
        return
    end
    local dim = if playing then "#c8c2e6" else "#c8c2e680"
    ui:text(shorten(title, h * 0.16, w), { x = x, y = h * 0.27, size = h * 0.16, w = w, color = if playing then "#ffffff" else "#ffffffa0" })
    ui:text(shorten(artist, h * 0.12, w), { x = x, y = h * 0.47, size = h * 0.12, w = w, color = dim })
    if progress then
        ui:bar(progress, { x = x, y = h * 0.72, w = math.min(w, ui.width * 0.42), h = math.max(4, h * 0.03), color = "#ffffff" })
    end
end)
