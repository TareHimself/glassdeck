local BANDS = 24
local heard = table.create(BANDS, 0)

local bass, mid, treble = 0, 0, 0
local beat, beatAge = 0, 10
local phase, hue = 0, 0

bridge.on.bands(function(bands)
    for i = 1, BANDS do
        heard[i] = bands.levels[i] or 0
    end
end)

local function average(from: number, to: number): number
    local sum = 0
    for i = from, to do
        sum += heard[i]
    end
    return sum / (to - from + 1)
end

-- Moves toward the goal, faster on the way up than the way down, the same at any frame rate.
local function ease(current: number, goal: number, dt: number, rise: number, fall: number): number
    local rate = if goal > current then rise else fall
    return current + (goal - current) * math.min(1, dt * rate)
end

script.on_frame(function(frame)
    local dt = frame.delta
    local nowBass = average(1, 6)

    -- A beat: the bass jumping well above where it has been.
    if nowBass > bass + 0.12 and beatAge > 0.25 then
        beat, beatAge = math.min(1, nowBass), 0
    end
    beatAge += dt
    beat *= math.exp(-dt * 4)

    bass = ease(bass, nowBass, dt, 12, 3)
    mid = ease(mid, average(7, 16), dt, 10, 2.5)
    treble = ease(treble, average(17, BANDS), dt, 14, 4)

    -- Louder music moves the picture faster, so it speeds up and settles without jumping.
    phase += dt * (0.35 + 2.4 * (bass + mid + treble) / 3)
    hue = (hue + dt * (0.008 + 0.04 * mid)) % 1

    inputs.bass, inputs.mid, inputs.treble = bass, mid, treble
    inputs.beat, inputs.beatAge = beat, beatAge
    inputs.phase, inputs.hue = phase, hue
end)
