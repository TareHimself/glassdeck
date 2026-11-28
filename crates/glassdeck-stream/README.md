# glassdeck-stream

Stream protocol v1: how a program attaches a named video stream to glassdeckd, so a theme can show it on the keyboard and the app can preview it. Plain Rust, no I/O beyond a TCP socket and reading `api.json`.

```rust
let (mut controls, sender) = glassdeck_stream::attach_local("my-stream", Mode::Live, Codec::H264)?;
sender.still(jpeg)?;
while let Some(control) = controls.recv()? {
    match control {
        Control::Activate => { /* start encoding: StreamStart, then a keyframe */ }
        Control::Keyframe => { /* a new viewer: send a keyframe soon */ }
        Control::Deactivate => { /* StreamEnd, stop encoding, stay attached */ }
    }
}
```

| Direction | Message | Body |
|---|---|---|
| program → glassdeckd | `Hello` | token, name, mode (live), codec (H.264 or HEVC) |
| glassdeckd → program | `Welcome` / `Refused` | reason |
| glassdeckd → program | `Activate`, `Deactivate`, `Keyframe` | |
| program → glassdeckd | `Still` | a JPEG for previews before any video |
| program → glassdeckd | `StreamStart` | codec, width, height |
| program → glassdeckd | `Frame` | seq, capture time (host Unix µs), keyframe flag, Annex B |
| program → glassdeckd | `StreamEnd` | |

Every message has an 8-byte header: version (1), kind, two zero bytes, the body length (`u32`, little-endian). Names are `a-z 0-9 . -`, up to 64 bytes; `monitor-*` is taken by built-in streams. glassdeckd refuses video that starts without a keyframe, goes backwards, or is larger than 2560x1440, and cuts its watchers off.

Only the app can preview an HEVC stream from another program if it can play HEVC, so prefer H.264.
