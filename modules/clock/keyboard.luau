type Settings = { seconds: boolean? }
type State = { seconds: boolean }

local module = {}

function module.init(settings: Settings): State
    ui:every(1, function()
        ui:redraw()
    end)
    return { seconds = settings.seconds == true }
end

function module.settings_changed(state: State, settings: Settings)
    state.seconds = settings.seconds == true
end

function module.draw(state: State, ui: Ui)
    local time = os.date(if state.seconds then "%H:%M:%S" else "%H:%M")
    ui:text(time, { x = 0, y = ui.height * 0.12, w = ui.width, size = ui.height * 0.5, align = "center" })
    ui:text(os.date("%A %d %B"), {
        x = 0,
        y = ui.height * 0.7,
        w = ui.width,
        size = ui.height * 0.14,
        align = "center",
        color = "#ffffffc0",
    })
end

return module
