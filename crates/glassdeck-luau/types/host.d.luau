-- host.luau: runs on the host (the machine running glassdeckd) and talks to its keyboard half over bridge.

declare function every(seconds: number, callback: () -> ()): ()

declare system: {
    cpu_percent: () -> number,
    memory_percent: () -> number,
}

declare host: {
    -- assets/host/<name>: stays on the host, so only host.luau has this table
    resource: (name: string) -> Resource,
}

-- This script: what it does when things happen to it
declare script: {
    -- Called when the package starts and whenever the user changes an option. Options and Changed
    -- come from options.luau (empty when the package has none); see options.d.luau.
    on_options: (callback: (options: Options, changed: Changed) -> ()) -> (),
}

-- Messages from the keyboard half (bridge.luau): bridge.on.<message>(callback)
declare bridge: { on: MessagesHeard }

-- The keyboard half: its resources (assets/keyboard/<name>), and keyboard.bridge.<message>(value) to send it one
declare keyboard: { resource: (name: string) -> Resource, bridge: MessagesSent }

-- Whether anything in a section of changed is true: if any(changed.colours) then ... end
declare function any<T>(section: T): boolean

-- sRGB from 0 to 1, as r, g, b, a in a vector (Luau's checker knows only 3-wide vector.create, so build colours here)
export type Color = vector

declare color: {
    -- "#rgb", "#rrggbb" or "#rrggbbaa"
    hex: (text: string) -> Color,
    -- 0 to 255 each; alpha defaults to 255
    rgb: (r: number, g: number, b: number, a: number?) -> Color,
    -- hue in degrees, saturation and value 0 to 1; alpha defaults to 1
    hsv: (h: number, s: number, v: number, a: number?) -> Color,
}

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

export type NowPlaying = {
    title: string,
    artist: string,
    album: string,
    playing: boolean,
    -- changes whenever the track does
    track: number,
    -- the cover as the player gave it (usually JPEG or PNG); image.fit makes it fit for live.put
    cover: string?,
    -- seconds into the track, and its length, when the player says
    position: number?,
    duration: number?,
}

declare media: {
    -- nil when nothing is playing or paused
    current: () -> NowPlaying?,
}

-- A request: method defaults to GET; timeout is in seconds (10 when left out, at most 30)
export type Request = {
    method: ("GET" | "POST" | "PUT" | "PATCH" | "DELETE")?,
    url: string,
    headers: { [string]: string }?,
    body: string?,
    timeout: number?,
}

export type Response = {
    -- a 2xx status
    ok: boolean,
    -- 0 when nothing answered
    status: number,
    -- names in lower case
    headers: { [string]: string },
    -- the bytes as they came, so image.fill(response.body, ...) works for pictures
    body: string,
    -- why nothing answered: a timeout, DNS, TLS
    error: string?,
}

-- HTTP to the hosts deck.json lists under "network". Each call waits, so use it inside every,
-- bridge.on and script.on_options; other callbacks keep running meanwhile.
declare net: {
    get: (url: string) -> Response,
    request: (request: Request) -> Response,
    -- all at once; the responses in the same order
    all: (requests: { string | Request }) -> { Response },
}

declare json: {
    -- null reads as nil
    decode: (text: string) -> any,
    -- a table with keys 1 to n is an array, else an object; an empty table is {}
    encode: (value: any) -> string,
}
