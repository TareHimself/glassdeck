-- options.luau's options, forwarded as they are (no host.luau)
type Options = { work_minutes: number, break_minutes: number }
type Phase = "work" | "break"
type State = { phase: Phase, ends: number, work: number, rest: number }

local module = {}

local function length(state: State): number
    return if state.phase == "work" then state.work else state.rest
end

function module.init(saved: State?): State
    ui:every(1, function()
        ui:redraw()
    end)
    if saved then
        return saved
    end
    return { phase = "work", ends = os.time() + 25 * 60, work = 25 * 60, rest = 5 * 60 }
end

-- New lengths apply from the next phase; the one running keeps its end.
function module.on_data(state: State, options: Options): boolean
    state.work, state.rest = options.work_minutes * 60, options.break_minutes * 60
    return true
end

-- Keeps the countdown running across a hot reload.
function module.unload(state: State): State
    return state
end

function module.draw(state: State, ui: Ui)
    local now = os.time()
    if now >= state.ends then
        state.phase = if state.phase == "work" then "break" else "work"
        state.ends = now + length(state)
    end
    local left = state.ends - now
    local size = math.min(ui.width, ui.height)
    local cx, cy, r = ui.width / 2, ui.height / 2, size / 2 - 10
    local color = if state.phase == "work" then "#ff6b5c" else "#5cd68a"
    ui:arc(cx, cy, r, 0, 360, { width = 8, color = "#ffffff30" })
    ui:arc(cx, cy, r, 0, 360 * left / length(state), { width = 8, color = color })
    ui:text(string.format("%d:%02d", left // 60, left % 60), {
        x = 0,
        y = cy - size * 0.14,
        w = ui.width,
        size = size * 0.22,
        align = "center",
    })
    ui:text(state.phase, { x = 0, y = cy + size * 0.1, w = ui.width, size = size * 0.1, align = "center", color = color })
end

return module
