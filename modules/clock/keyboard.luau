local seconds = false

script.every(1, function()
    script.redraw()
end)

-- options.luau's options, forwarded as they are (no host.luau)
bridge.on.options(function(options)
    seconds = options.seconds
end)

script.on_draw(function(ui: Ui)
    local time = os.date(if seconds then "%H:%M:%S" else "%H:%M")
    ui:column({ padding = ui.height * 0.1, gap = ui.height * 0.04, align = "stretch" }, function()
        ui:text(time, { size = "fill", align = "center", grow = 3 })
        ui:text(os.date("%A %d %B"), { size = "fill", align = "center", color = "#ffffffc0", grow = 1 })
    end)
end)
