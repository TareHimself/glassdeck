-- options.luau's options, forwarded as they are (no host.luau)
type Options = { seconds: boolean }

local seconds = false

widget.every(1, function()
    widget.redraw()
end)

widget.on_data(function(options: Options)
    seconds = options.seconds
end)

widget.on_draw(function(ui: Ui)
    local time = os.date(if seconds then "%H:%M:%S" else "%H:%M")
    ui:text(time, { x = 0, y = ui.height * 0.12, w = ui.width, size = ui.height * 0.5, align = "center" })
    ui:text(os.date("%A %d %B"), {
        x = 0,
        y = ui.height * 0.7,
        w = ui.width,
        size = ui.height * 0.14,
        align = "center",
        color = "#ffffffc0",
    })
end)
