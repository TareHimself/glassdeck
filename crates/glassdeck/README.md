# glassdeck

The CLI. Anything that reaches the keyboard goes through [glassdeckd](../glassdeckd) over its local API, using the types in [glassdeck-api](../glassdeck-api). Packing, converting, recording and listing monitors run in the CLI itself. Windows only.

## Commands

| Command | What it does |
|---|---|
| `status` | The Link, the background and where it runs |
| `streams` | The streams glassdeckd can show, such as `monitor-q27g4zd` |
| `bg [choice]` | Shows or sets the background: `gradient`, a `.deckbg` (uploaded first if it's a local file), or `stream:<name>` |
| `deploy <file>` | Puts a `.deckbg`, `.deckmod` or profile `.json` on the keyboard |
| `options [--widget N] [path value \| path --reset]` | Shows or sets the selected background's options, or a widget's. Values are typed as you'd say them: `on`, `0.5`, `#ff8000`, `ember` |
| `pack <folder> [--out] [--watch]` | Packs a module folder into a `.deckmod`, or a canvas folder into a `.deckbg`. Scripts are type-checked first, and `--watch` repacks on every change |
| `convert <input>` | Turns a video, or a web page folder or `.html`, into a `.deckbg` |
| `monitors` | Lists monitors with their index and EDID name |
| `record <monitor>` | Records a monitor to an MP4 |

`options` turns typed text into the JSON value an option's kind takes (`parse_typed`), so the API itself only ever takes JSON.

## Native libraries

`pack` for canvases loads Slang and the Luau checker from `GLASSDECK_NATIVE`, else from beside `glassdeck.exe`. Build them with [native/build.py](../../native/README.md).
