inputs.night = color.hex("#0f0a29")
inputs.violet = color.hex("#3d1c73")
inputs.rose = color.hex("#d66b78")
inputs.teal = color.hex("#0d5c85")

local speed = 0.08
local quarter = math.pi / 2

script.on_frame(function(frame)
    local t = frame.time * speed
    inputs.roseAt = drift(t, vector.create(0.5, 0.3), vector.create(0.6, 0.25), vector.create(1.3, 0.9))
    inputs.tealAt = drift(t, vector.create(1.3, 0.7), vector.create(0.5, 0.2), vector.create(0.7, 1.1), vector.create(quarter, -quarter))
    inputs.glowAt = drift(t, vector.create(0.9, 0.5), vector.create(0.7, 0.3), vector.create(0.5, 0.8), vector.create(2, 1 - quarter))
end)
