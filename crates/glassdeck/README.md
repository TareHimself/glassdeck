# glassdeck

The CLI. Most of what reaches the keyboard goes through [glassdeckd](../glassdeckd) over its local API, using the types in [glassdeck-api](../glassdeck-api). Packing, converting, recording and listing monitors run in the CLI itself. Windows only.

## Commands

| Command | What it does |
|---|---|
| `status` | The Link, the stream and the active theme's background |
| `streams` | The streams glassdeckd can show, such as `monitor-q27g4zd` |
| `background [choice]` (alias `bg`) | Shows or sets the active theme's background: `gradient`, a package id glassdeckd already holds, a `.deck` file (pushed with the theme), or `stream:<name>`. It edits the theme and pushes it |
| `theme [file] [--library dir]` | Shows the active theme as JSON, or pushes a theme file with the packages it names from a library folder (`<id>.deck`, default the current folder) |
| `options [--widget N] [path value \| path --reset]` | Shows or sets the theme's background options, or a widget's. It runs the package's `options.luau` here and pushes the theme. Values are typed as you'd say them: `on`, `0.5`, `#ff8000`, `ember` |
| `pack <folder> [--out] [--watch]` | Packs a widget or canvas folder (with its `deck.json`) into a `.deck`. Scripts are type-checked first, and `--watch` repacks a canvas on every change |
| `convert <input>` | Turns a video, or a web page folder or `.html`, into a background `.deck`, written as `converted.<name>.deck` in the current folder |
| `monitors` | Lists monitors with their index and EDID name |
| `record <monitor>` | Records a monitor to an MP4 |

`options` turns typed text into the JSON value an option's kind takes (`parse_typed`), so the theme only ever holds JSON.

### Straight to the keyboard over USB

These don't go through glassdeckd. They're for getting onto a stock keyboard and looking after its key scanner.

| Command | What it does |
|---|---|
| `scanner version \| backup \| check-backup \| bootloader \| flash \| restore` | The key scanner (RP2040); see [firmware/README.md](../../firmware/README.md) |
| `keyboard versions` | The stock bridge's bridge, display and OS versions, and free space |
| `keyboard logs` | The end of Glassdeck's own logs on the keyboard: the bootstrap's install, the launcher, the receiver |
| `keyboard install <file.ipk>` | Uploads one `.ipk` to `/home/root`, where the stock system installs it as root, and waits for it to be picked up |
| `keyboard probe` | Installs `target/packages/glassdeck-probe.ipk` and saves its report as `glassdeck-probe.txt` |
| `keyboard backup <dir>` | Installs `glassdeck-backup.ipk`, waits for it, downloads every piece into a new folder and checks each partition against its SHA-256 |

`keyboard` offers only these steps. It never uploads anything but a checked `.ipk` to `/home/root`, reads only the probe report, the backup folder and Glassdeck's own logs, and deletes only the last probe report and the backup's `DONE` marker. The bridge itself can move any file under `/home`; this keeps that out of everyday reach.

## Native libraries

`pack` for canvases loads Slang and the Luau checker from `GLASSDECK_NATIVE`, else from beside `glassdeck.exe`. Build them with [native/build.py](../../native/README.md).
