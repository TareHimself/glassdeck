type Data = { title: string?, artist: string?, playing: boolean? }

local cover = live.resource("cover")

local title, artist, playing = "", "", false

widget.on_data(function(data: Data)
    title, artist, playing = data.title or "", data.artist or "", data.playing == true
end)

widget.on_draw(function(ui: Ui)
    if title == "" then
        ui:text("Nothing playing", { x = 0, y = ui.height / 2 - 12, size = 20, color = "#ffffff80" })
        return
    end
    local side = ui.height
    ui:rect(0, 0, side, side, { fill = "#ffffff14", radius = 8 })
    ui:image(cover, { x = 0, y = 0, w = side, h = side })
    local x = side + 16
    local w = ui.width - x
    local dim = if playing then "#ffffffb0" else "#ffffff70"
    ui:text(title, { x = x, y = side / 2 - 36, size = 28, w = w })
    ui:text(artist, { x = x, y = side / 2 + 4, size = 18, w = w, color = dim })
end)
