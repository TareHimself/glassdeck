type Data = { cpu: number, memory: number }

local HISTORY = 60

local cpu, shown, memory = 0, 0, 0
local history: { number } = {}
local animating = false

-- Eases the bar toward the latest reading, then stops asking for frames.
local function animate()
    if animating then
        return
    end
    animating = true
    local function step(dt: number)
        shown += (cpu - shown) * math.min(1, dt * 8)
        if math.abs(cpu - shown) < 0.2 then
            shown = cpu
            animating = false
        else
            widget.request_frame(step)
        end
    end
    widget.request_frame(step)
end

widget.on_data(function(data: Data)
    cpu = data.cpu
    memory = data.memory
    table.insert(history, data.cpu)
    if #history > HISTORY then
        table.remove(history, 1)
    end
    animate()
end)

widget.on_draw(function(ui: Ui)
    ui:text("CPU", { x = 16, y = 12, size = 16, color = "#ffffffb0" })
    ui:text(string.format("%d%%", math.round(shown)), { x = 16, y = 32, size = 36 })
    ui:text(string.format("RAM %d%%", math.round(memory)), {
        x = ui.width - 16,
        y = 14,
        size = 14,
        align = "right",
        color = "#ffffffb0",
    })
    ui:graph(history, { x = 112, y = 40, w = ui.width - 128, h = ui.height - 76, min = 0, max = 100, color = "#7cc4ff" })
    ui:bar(shown / 100, { x = 16, y = ui.height - 24, w = ui.width - 32, h = 8, color = "#7cc4ff" })
end)
