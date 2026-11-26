# glassdeck-package

The package formats: `.deckbg` (backgrounds, including canvases) and `.deckmod` (modules). Used on both sides.

Both are a ZIP of stored entries only, each aligned to 4 KB (`ALIGNMENT`), no ZIP64. Entries are read in place, never extracted. Modules and backgrounds refuse each other's files.

## `.deckbg`

| Type | Role |
|---|---|
| `Background` | An open `.deckbg`: `open`, `manifest`, `entry`, `read_entry`, `read_range` |
| `BackgroundWriter` | `create(path, manifest)`, `add_bytes`, `add_file`, `finish` |
| `EntryReader` | `Read` over one entry |
| `Manifest` | `manifest.json`: `format` (`FORMAT_VERSION`), `title`, `Origin`, `Content`, optional `preview` |
| `Content` | `Video` (H.264 or HEVC at 1920x1080), `Web` (page entry plus properties), `Canvas` (a plan entry) |
| `Origin` | `WallpaperEngine`, `Converted`, or `Packed` (with a `source_digest` of what was packed) |
| `check(ProbedVideo)` | Whether a video meets the background spec, as `VideoFormat` or `SpecMismatch` |

### Canvases

A canvas `.deckbg` holds what `glassdeck pack` produced, so the keyboard runs it without Slang:

- `canvas/plan.json` (`CanvasPlan`): API version, scale, frame rate, `[Inputs]` fields (`InputField`, `InputKind`), and which scripts are present.
- `canvas/fragment.essl`, plus optional `canvas/keyboard.luau`, `canvas/options.luau`, `canvas/host.luau`.
- `CanvasFiles::read` / `from_bytes`: what the keyboard renderer needs.
- `canvas_sources` and `source_digest`: detect a stale pack.

## `.deckmod`

| Type | Role |
|---|---|
| `ModuleManifest` | Checked `module.json` (format `MODULE_FORMAT_VERSION`): id, name, version, `Size`, `Resize`, `host` |
| `ModulePackage` | The whole module as the host keeps it: scripts, all resources |
| `KeyboardModule` | What the keyboard gets: `module.json`, `keyboard.luau`, `assets/keyboard/` |
| `ModuleWriter`, `pack_folder` | Pack a module folder |

`ModulePackage::write_keyboard_copy` writes the keyboard half and drops the host half. The JSON schema for `module.json` is in `schemas/module.schema.json`.

## Resources

`Resource` = `ResourceKind` (`Keyboard` or `Host`) + `ResourceName`. A name resolves only inside its module's `assets/<kind>/`. `literal_resources(script)` finds the names a script uses.

## Used by

glassdeckd, glassdeck, glassdeck-capture, glassdeck-convert, glassdeck-wallpaper-engine, glassdeck-slang, glassdeck-luau, glassdeck-gpu, glassdeck-receiver.

## Platform

Portable. Depends on glassdeck-proto (for `Codec`).

## Test

```sh
cargo test -p glassdeck-package
```

No ignored tests.
