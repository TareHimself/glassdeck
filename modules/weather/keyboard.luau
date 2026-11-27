local celsius: number? = nil
local sky = ""

-- WMO weather codes, as Open-Meteo gives them
local function describe(code: number): string
    if code == 0 then
        return "Clear"
    elseif code <= 3 then
        return "Cloudy"
    elseif code <= 48 then
        return "Fog"
    elseif code <= 67 then
        return "Rain"
    elseif code <= 77 then
        return "Snow"
    elseif code <= 82 then
        return "Showers"
    end
    return "Storm"
end

bridge.on.weather(function(weather)
    celsius = weather.celsius
    sky = describe(weather.code)
end)

script.on_draw(function(ui: Ui)
    local temperature = if celsius then string.format("%d°", math.round(celsius)) else "--"
    ui:text(temperature, { x = 0, y = ui.height * 0.12, w = ui.width, size = ui.height * 0.5, align = "center" })
    ui:text(sky, { x = 0, y = ui.height * 0.7, w = ui.width, size = ui.height * 0.14, align = "center", color = "#ffffffc0" })
end)
