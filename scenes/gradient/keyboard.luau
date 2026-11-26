scene.settings = {
    night = setting.color("#0f0a29"),
    violet = setting.color("#3d1c73"),
    rose = setting.color("#d66b78"),
    teal = setting.color("#0d5c85"),
    speed = setting.range(0.08, 0, 0.5),
}

function scene.on_settings(settings)
    inputs.night, inputs.violet = settings.night, settings.violet
    inputs.rose, inputs.teal = settings.rose, settings.teal
end

function scene.on_frame(frame, settings)
    local t = frame.time * settings.speed
    inputs.roseAt = drift(t, vector(0.5, 0.3), vector(0.6, 0.25), vector(1.3, 0.9))
    inputs.tealAt = drift(t, vector(1.3, 0.7), vector(0.5, 0.2), vector(0.7, 1.1))
    inputs.glowAt = drift(t, vector(0.9, 0.5), vector(0.7, 0.3), vector(0.5, 0.8))
end
