type Data = { cpu: number, memory: number }
type State = { cpu: number, shown: number, memory: number, history: { number }, animating: boolean }

local HISTORY = 60

local module = {}

-- Eases the bar toward the latest reading, then stops asking for frames.
local function animate(state: State)
    if state.animating then
        return
    end
    state.animating = true
    local function step(dt: number)
        state.shown += (state.cpu - state.shown) * math.min(1, dt * 8)
        if math.abs(state.cpu - state.shown) < 0.2 then
            state.shown = state.cpu
            state.animating = false
        else
            ui:request_frame(step)
        end
    end
    ui:request_frame(step)
end

function module.init(): State
    return { cpu = 0, shown = 0, memory = 0, history = {}, animating = false }
end

function module.on_data(state: State, data: Data): boolean
    state.cpu = data.cpu
    state.memory = data.memory
    table.insert(state.history, data.cpu)
    if #state.history > HISTORY then
        table.remove(state.history, 1)
    end
    animate(state)
    return true
end

function module.draw(state: State, ui: Ui)
    ui:text("CPU", { x = 16, y = 12, size = 16, color = "#ffffffb0" })
    ui:text(string.format("%d%%", math.round(state.shown)), { x = 16, y = 32, size = 36 })
    ui:text(string.format("RAM %d%%", math.round(state.memory)), {
        x = ui.width - 16,
        y = 14,
        size = 14,
        align = "right",
        color = "#ffffffb0",
    })
    ui:graph(state.history, { x = 112, y = 40, w = ui.width - 128, h = ui.height - 76, min = 0, max = 100, color = "#7cc4ff" })
    ui:bar(state.shown / 100, { x = 16, y = ui.height - 24, w = ui.width - 32, h = 8, color = "#7cc4ff" })
end

return module
