# glassdeck-convert

Turns a video file or a web page folder into a background `.deck`. Host only, Windows only. It knows nothing about where the input came from: importers such as glassdeck-wallpaper-engine pass their own id, title and origin.

## API

| Item | Role |
|---|---|
| `video(source, out, details, settings)` | Keeps the video as it is if it meets the background spec (H.264 or HEVC at 1920x1080), else re-encodes it to HEVC |
| `web(folder, entry, properties, out, details)` | Packs the whole folder under `web/`, with `entry` as the page to open |
| `Details` | Id, title, `Origin`, optional preview image |
| `EncodeSettings` | Re-encode bitrate |
| `default_packages_dir()` | `%APPDATA%\Glassdeck\packages`, glassdeckd's own folder |
| `video::{probe, transcode, cover}` | Media Foundation probe and re-encode. `cover` crops to fill the panel |
| `web_media::{probe, plan, convert}` | Media inside web pages |

### Media in web pages

Videos must decode in hardware in CEF on the host and in WebKit on the keyboard.

- VP8 and VP9 (yuv420p, within the size limit) are kept, with any audio dropped.
- Any other video becomes VP9.
- Audio files become silence of the same length, so every file name the page refers to still exists.

## Used by

glassdeck (the CLI's `convert`), glassdeck-capture, glassdeck-wallpaper-engine.

## Platform

- Windows only: video uses Media Foundation.
- Web media conversion needs `ffmpeg` and `ffprobe` on `PATH`.

## Test

```sh
cargo test -p glassdeck-convert
```

Unit tests cover crop geometry, media planning and folder listing, and need neither ffmpeg nor a GPU. No ignored tests.
