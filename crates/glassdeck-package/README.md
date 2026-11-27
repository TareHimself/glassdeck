# glassdeck-package

The package format: `.deck`, for backgrounds (video, web, canvas or image) and widgets. Used on both sides.

A `.deck` is a ZIP of stored entries only, each aligned to 4 KB (`ALIGNMENT`), no ZIP64. Entries are read in place, never extracted.

## `.deck`

| Type | Role |
|---|---|
| `Package` | An open `.deck`: `open(path)`, `built_in(bytes)` for one built into a program, `manifest`, `entry`, `read_entry`, `read_range`, and the scripts and resources by name |
| `PackageWriter` | `create(path, manifest)`, `add_bytes`, `add_file`, `finish` |
| `EntryReader` | `Read` over one entry |
| `Manifest` | `manifest.json`: `format` (`FORMAT_VERSION`, 4), `id` (`PackageId`), `title`, `version`, `Origin`, optional `preview` (packed folders carry none: the app keeps stills itself), `network` (the hosts `host.luau` may reach, as `NetworkHost`), and `type` with its `details` (`Kind`) |
| `Kind` | `Background(Content)` or `Widget(WidgetSize)` |
| `Content` | `Video` (`file` and its `VideoFormat`: H.264 or HEVC at 1920x1080), `Web` (`entry` plus `properties`), `Canvas`, `Image` (`file`, a JPEG at the panel's size) |
| `WidgetSize` | `size`, plus optional `min` and `max` (`Resize`) |
| `PackageId` | `{tld}.{entity}.{name}` in lowercase, such as `local.user.clock`: what `deck.json` and `manifest.json` call the package. Unique within its type |
| `QualifiedId` | `{type}.{id}`, such as `widget.local.user.clock`: what Glassdeck keys packages by, and the file name `<qualified id>.deck` (`check_file_name`) |
| `Origin` | `WallpaperEngine`, `Converted`, or `Packed` (with a `source_digest` of what was packed) |
| `check(ProbedVideo)` | Whether a video meets the background spec, as `VideoFormat` or `SpecMismatch` |

Scripts are found by file name: `keyboard.luau`, `options.luau`, `host.luau`, `bridge.luau`. `Package::write_keyboard_copy` writes the package as the keyboard gets it, leaving out `options.luau`, `host.luau` and `assets/host/`.

### Canvases

A canvas `.deck` holds what `glassdeck pack` produced, so the keyboard runs it without Slang:

- `details` (`Canvas`): `api`, `scale`, `frame_rate`, the `[Inputs]` fields (`InputField`, `InputKind`), `targets` (`RenderTarget`: name, scale, `TargetFormat`, feedback) and `passes` (`Pass`: the target it writes). At most `MAX_PASSES` and `MAX_TARGETS`, 4 each.
- `shader.essl`, one `pass{n}.essl` per pass (`pass_shader(n)`), plus optional `keyboard.luau`, `options.luau`, `host.luau`, `bridge.luau`.
- `CanvasFiles::read`: what the keyboard renderer needs.
- `canvas_sources` and `source_digest`: detect a stale pack.

## Source folders

| Item | Role |
|---|---|
| `DeckSource` | The hand-written `deck.json` (`DECK_SOURCE`): `id`, `title`, `version`, `type`, a widget's `details`, `preview_at` (seconds in, for the app's still) and `network` |
| `pack_folder` | Pack a widget folder |
| `read_script`, `is_blank_script` | A script from a source folder; a blank one (only whitespace and comments) is the same as a missing one |

`glassdeck pack` fills in the rest of the manifest, so a canvas's `deck.json` has no details. The JSON schema for `deck.json` is in `schemas/deck.schema.json`.

## Resources

`Resource` = `ResourceKind` (`Keyboard` or `Host`) + `ResourceName`. A name resolves only inside its package's `assets/<kind>/`. `literal_resources(script)` finds the names a script uses.
