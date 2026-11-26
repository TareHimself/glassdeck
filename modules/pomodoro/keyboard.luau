-- options.luau's options, forwarded as they are (no host.luau)
type Options = { work_minutes: number, break_minutes: number }
type Phase = "work" | "break"
type Timer = { phase: Phase, ends: number, work: number, rest: number }

-- Keeps the countdown running across a hot reload.
local timer: Timer = widget.saved or { phase = "work", ends = os.time() + 25 * 60, work = 25 * 60, rest = 5 * 60 }

local function length(): number
    return if timer.phase == "work" then timer.work else timer.rest
end

widget.every(1, function()
    widget.redraw()
end)

-- New lengths apply from the next phase; the one running keeps its end.
widget.on_data(function(options: Options)
    timer.work, timer.rest = options.work_minutes * 60, options.break_minutes * 60
end)

widget.on_unload(function()
    return timer
end)

widget.on_draw(function(ui: Ui)
    local now = os.time()
    if now >= timer.ends then
        timer.phase = if timer.phase == "work" then "break" else "work"
        timer.ends = now + length()
    end
    local left = timer.ends - now
    local size = math.min(ui.width, ui.height)
    local cx, cy, r = ui.width / 2, ui.height / 2, size / 2 - 10
    local color = if timer.phase == "work" then "#ff6b5c" else "#5cd68a"
    ui:arc(cx, cy, r, 0, 360, { width = 8, color = "#ffffff30" })
    ui:arc(cx, cy, r, 0, 360 * left / length(), { width = 8, color = color })
    ui:text(string.format("%d:%02d", left // 60, left % 60), {
        x = 0,
        y = cy - size * 0.14,
        w = ui.width,
        size = size * 0.22,
        align = "center",
    })
    ui:text(timer.phase, { x = 0, y = cy + size * 0.1, w = ui.width, size = size * 0.1, align = "center", color = color })
end)
