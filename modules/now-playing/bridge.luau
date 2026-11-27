type Track = { title: string, artist: string, playing: boolean, position: number?, duration: number? }

-- Nothing playing when `track` is nil
export type nowPlaying = KeyboardState<{ track: Track? }>
