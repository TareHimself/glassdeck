scene.settings = {
    night = setting.color("#0f0a29"),
    violet = setting.color("#3d1c73"),
    rose = setting.color("#d66b78"),
    teal = setting.color("#0d5c85"),
    speed = setting.range(0.08, 0, 0.5),
}

local quarter = math.pi / 2

scene.on_settings(function(settings)
    inputs.night, inputs.violet = settings.night, settings.violet
    inputs.rose, inputs.teal = settings.rose, settings.teal
end)

scene.on_frame(function(frame, settings)
    local t = frame.time * settings.speed
    inputs.roseAt = drift(t, vector.create(0.5, 0.3), vector.create(0.6, 0.25), vector.create(1.3, 0.9))
    inputs.tealAt = drift(t, vector.create(1.3, 0.7), vector.create(0.5, 0.2), vector.create(0.7, 1.1), vector.create(quarter, -quarter))
    inputs.glowAt = drift(t, vector.create(0.9, 0.5), vector.create(0.7, 0.3), vector.create(0.5, 0.8), vector.create(2, 1 - quarter))
end)
