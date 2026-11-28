# glassdeck-wallpaper-engine-stream

Attaches `wallpaper-engine` to glassdeckd with [glassdeck-stream](../glassdeck-stream): the wallpaper Wallpaper Engine shows on its first monitor, any type (scene, video, web).

- While someone watches, it asks Wallpaper Engine to play that wallpaper in a window of its own at 1920x1080 (`wallpaper64.exe -control openWallpaper -playInWindow`), moves the window off every monitor, mutes Wallpaper Engine in the volume mixer, captures the window and encodes H.264 on the GPU.
- Choosing another wallpaper in Wallpaper Engine switches the stream within a couple of seconds.
- It attaches while Wallpaper Engine runs and glassdeckd is up, and tries again every 5 s otherwise.

```sh
cargo run -p glassdeck-wallpaper-engine-stream --release
```

Then choose `wallpaper-engine` on the app's Overview, or `glassdeck bg stream:wallpaper-engine`.
