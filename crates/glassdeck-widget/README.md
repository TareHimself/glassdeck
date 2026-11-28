# glassdeck-widget

A widget's keyboard half. `Instance::start` runs `keyboard.luau` in its own VM with the injected `script` global, hears its host half through `bridge.on.<msg>` and sends with `host.bridge.<msg>`, and draws its tiles on the CPU in Noto Sans JP Bold (`fonts/`, OFL). The receiver runs it on the keyboard; the desktop app runs it for the editor's preview.

## Modules

| Module | What it does |
|---|---|
| `instance` | One placed widget: its VM, the `script` callbacks, `bridge` messages, saved state and drawing |
| `ui` | The drawing-only `ui` passed to `script.on_draw`: absolute calls at the top level, `row`, `column`, `stack` and `box` containers inside |
| `layout` | Lays a container tree out with `taffy`, a pure function of the tree and what its text and images measure |
| `draw` | Rasterizing shapes, text and images into a tile |
| `text` | Laying text out in a width: ellipsis, shrink, wrap, scroll and clip, and how far scrolling text has panned |
| `live` | `LiveCache`, the latest frame of each live resource |
| `installing` | The tile shown while a package is still arriving |

## Test

The tests start every example widget in `modules/` and draw it.
