-- What the speakers play, 60 times a second; silence goes once rather than over and over.
local BANDS = 24
local silent = false

every(1 / 60, function()
    local levels = audio.bands(BANDS)
    local quiet = true
    for _, level in levels do
        if level > 0 then
            quiet = false
            break
        end
    end
    if quiet and silent then
        return
    end
    silent = quiet
    keyboard.bridge.bands({ levels = levels })
end)
