# glassdeck-wallpaper-engine

Imports Wallpaper Engine items already in the local Steam workshop folder ("bring your own install"). Host only, Windows only. Nothing is downloaded.

It is a library and a binary of the same name.

## Binary

```sh
glassdeck-wallpaper-engine list                      # subscribed items, their type, and why any can't become a background
glassdeck-wallpaper-engine import <id-or-folder> [--out file.deck] [--mbps 12]
```

The default output is `wallpaper-engine.<id>.deck` in the current folder.

## API

| Item | Role |
|---|---|
| `library::{subscribed, find, workshop_dirs}`, `Item` | Workshop items for app `431960` |
| `project::{read, parse}`, `Project`, `Kind` | An item's `project.json`. `Kind` is `Video`, `Web`, `Scene`, `Application`, `Preset` or `Unknown` |
| `why_unsupported(kind)` | Why a kind can't become a background |
| `convert(item, project, out, settings)` | Video and web items to a `.deck`, through glassdeck-convert |
| `init_script(properties)` | JavaScript standing in for Wallpaper Engine's hooks: user property defaults go to `applyUserProperties`, audio and media listeners stay silent |
| `package_id(item)`, `package_path(dir, item)` | An item's id, `wallpaper-engine.<workshop id>`, and where its package goes |
| `steam::{steam_root, library_folders, parse_vdf}` | Finds Steam and its library folders |

Scenes can't be converted. glassdeck-capture streams them live instead, rendered by Wallpaper Engine itself.

## Used by

glassdeck-capture (wallpaper and scene sources, web background init script).

## Platform

Windows only. Reads the Steam install location from the registry, and conversion uses Media Foundation through glassdeck-convert.

## Test

```sh
cargo test -p glassdeck-wallpaper-engine
```

Tests cover `project.json` parsing, VDF parsing and the init script. No ignored tests.
