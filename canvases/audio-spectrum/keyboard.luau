local BANDS = 24
local heard = table.create(BANDS, 0)
local shown = table.create(BANDS, 0)

bridge.on.bands(function(bands)
    for i = 1, BANDS do
        heard[i] = bands.levels[i] or 0
    end
end)

-- Bars jump up to a beat and fall back slowly, like a meter.
script.on_frame(function(frame)
    local rise = math.min(1, frame.delta * 30)
    local fall = math.min(1, frame.delta * 3)
    for i = 1, BANDS do
        local goal = heard[i]
        shown[i] += (goal - shown[i]) * (if goal > shown[i] then rise else fall)
    end
    inputs.levels = shown
end)
