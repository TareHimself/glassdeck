-- Asks Open-Meteo for the current weather when the place changes, and every ten minutes.
local place = { latitude = 51.5, longitude = -0.13 }

local function refresh()
    local url = string.format(
        "https://api.open-meteo.com/v1/forecast?latitude=%.3f&longitude=%.3f&current=temperature_2m,weather_code",
        place.latitude,
        place.longitude
    )
    local response = net.get(url)
    if not response.ok then
        print("weather:", response.status, response.error)
        return
    end
    local now = json.decode(response.body).current
    keyboard.bridge.weather({ celsius = now.temperature_2m, code = now.weather_code })
end

script.on_options(function(options)
    place = { latitude = options.latitude, longitude = options.longitude }
    refresh()
end)

every(600, refresh)
