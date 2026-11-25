local module = {}

local cover = live.resource("cover")

type State = { title: string, artist: string, playing: boolean }

function module.init(): State
    return { title = "", artist = "", playing = false }
end

function module.on_data(state: State, data: { title: string?, artist: string?, playing: boolean? }): boolean
    local title, artist, playing = data.title or "", data.artist or "", data.playing == true
    local changed = title ~= state.title or artist ~= state.artist or playing ~= state.playing
    state.title, state.artist, state.playing = title, artist, playing
    return changed
end

function module.draw(state: State, ui: Ui)
    if state.title == "" then
        ui:text("Nothing playing", { x = 0, y = ui.height / 2 - 12, size = 20, color = "#ffffff80" })
        return
    end
    local side = ui.height
    ui:rect(0, 0, side, side, { fill = "#ffffff14", radius = 8 })
    ui:image(cover, { x = 0, y = 0, w = side, h = side })
    local x = side + 16
    local w = ui.width - x
    local dim = if state.playing then "#ffffffb0" else "#ffffff70"
    ui:text(state.title, { x = x, y = side / 2 - 36, size = 28, w = w })
    ui:text(state.artist, { x = x, y = side / 2 + 4, size = 18, w = w, color = dim })
end

return module
