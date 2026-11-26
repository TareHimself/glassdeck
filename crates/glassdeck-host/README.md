# glassdeck-host

A widget's or background's host half. `HostSide::start` runs `options.luau`, then `host.luau` in its own VM, or forwards the options as-is when there is no `host.luau`. What the script sends is taken with `take_sent`, and live resources with `take_live`.

## Modules

| Module | What it does |
|---|---|
| `api` | The host-only Luau APIs: `live`, `image` and `media` |
| `media` | Now playing, from Windows' media session |

## Used by

glassdeckd (the active theme) and the desktop app (the editor's preview).

## Test

```sh
cargo test -p glassdeck-host
```
