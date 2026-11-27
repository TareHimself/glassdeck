# glassdeck-widget

A widget's keyboard half. `Instance::start` runs `keyboard.luau` in its own VM with the injected `widget` global, and draws its tiles on the CPU in Noto Sans JP Bold (`fonts/`, OFL).

## Modules

| Module | What it does |
|---|---|
| `instance` | One placed widget: its VM, the `widget` callbacks, saved state and drawing |
| `ui` | The drawing-only `ui` passed to `widget.on_draw` |
| `draw` | Rasterizing shapes, text and images into a tile |
| `text` | Laying text out in a width: ellipsis, shrink, wrap, scroll and clip, and how far scrolling text has panned |
| `live` | `LiveCache`, the latest frame of each live resource |
| `installing` | The tile shown while a package is still arriving |

## Used by

glassdeck-receiver (on the keyboard) and the desktop app (the editor's preview).

## Test

```sh
cargo test -p glassdeck-widget
```

The tests start every example widget in `modules/` and draw it.
