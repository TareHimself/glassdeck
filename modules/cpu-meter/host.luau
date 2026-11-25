every(1, function()
    send({ cpu = system.cpu_percent(), memory = system.memory_percent() })
end)
