# glassdeck

The CLI. Anything that reaches the keyboard goes through [glassdeckd](../glassdeckd) over its local API, using the types in [glassdeck-api](../glassdeck-api). Packing, converting, recording and listing monitors run in the CLI itself. Windows only.

## Commands

| Command | What it does |
|---|---|
| `status` | The Link, the stream and the active theme's background |
| `streams` | The streams glassdeckd can show, such as `monitor-q27g4zd` |
| `background [choice]` (alias `bg`) | Shows or sets the active theme's background: `gradient`, a package id, a `.deck` file (deployed first), or `stream:<name>`. It edits the theme and pushes it |
| `deploy <file>` | Puts a `.deck` on the keyboard |
| `options [--widget N] [path value \| path --reset]` | Shows or sets the theme's background options, or a widget's. It runs the package's `options.luau` here and pushes the theme. Values are typed as you'd say them: `on`, `0.5`, `#ff8000`, `ember` |
| `pack <folder> [--out] [--watch]` | Packs a widget or canvas folder (with its `deck.json`) into a `.deck`. Scripts are type-checked first, and `--watch` repacks a canvas on every change |
| `convert <input>` | Turns a video, or a web page folder or `.html`, into a background `.deck`, written as `converted.<name>` into the packages folder |
| `monitors` | Lists monitors with their index and EDID name |
| `record <monitor>` | Records a monitor to an MP4 |

`options` turns typed text into the JSON value an option's kind takes (`parse_typed`), so the theme only ever holds JSON.

## Native libraries

`pack` for canvases loads Slang and the Luau checker from `GLASSDECK_NATIVE`, else from beside `glassdeck.exe`. Build them with [native/build.py](../../native/README.md).
