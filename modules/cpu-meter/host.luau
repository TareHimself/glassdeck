every(1, function()
    keyboard.bridge.usage({ cpu = system.cpu_percent(), memory = system.memory_percent() })
end)
