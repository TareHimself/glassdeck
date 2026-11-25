-- Sends the track every second, and the cover only when the track changes.
local COVER_SIDE = 256

local sent_track: number? = nil

every(1, function()
    local now = media.current()
    if now == nil then
        if sent_track ~= nil then
            live.clear("cover")
            sent_track = nil
        end
        send({})
        return
    end
    if now.track ~= sent_track then
        sent_track = now.track
        if now.cover ~= nil then
            live.put("cover", image.fill(now.cover, COVER_SIDE, COVER_SIDE))
        else
            live.clear("cover")
        end
    end
    send({ title = now.title, artist = now.artist, playing = now.playing })
end)
