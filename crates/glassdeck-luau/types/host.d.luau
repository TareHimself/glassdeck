-- host.luau: runs on the host (the machine running glassdeckd) and sends data to its keyboard half's on_data.

declare function every(seconds: number, callback: () -> ()): ()

-- The latest value wins: a send replaces anything of this widget's not yet on the Link.
declare function send(data: Data): ()

declare system: {
    cpu_percent: () -> number,
    memory_percent: () -> number,
}

-- assets/host/<name>: stays on the host, so only host.luau has this table
declare host: { resource: (name: string) -> Resource }

-- Resources for the keyboard half, drawn there with live.resource(name). Latest wins per name.
declare live: {
    -- a PNG of at most 1 MB; image.fit and image.fill make one
    put: (name: string, png: string) -> (),
    clear: (name: string) -> (),
}

declare image: {
    -- a PNG or JPEG scaled to fit within w x h, keeping its shape, as a PNG
    fit: (bytes: string, w: number, h: number) -> string,
    -- a PNG or JPEG scaled and cropped to exactly w x h, keeping its shape, as a PNG
    fill: (bytes: string, w: number, h: number) -> string,
}

type NowPlaying = {
    title: string,
    artist: string,
    album: string,
    playing: boolean,
    -- changes whenever the track does
    track: number,
    -- the cover as the player gave it (usually JPEG or PNG); image.fit makes it fit for live.put
    cover: string?,
}

declare media: {
    -- nil when nothing is playing or paused
    current: () -> NowPlaying?,
}
