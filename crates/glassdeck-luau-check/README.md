# glassdeck-luau-check

Luau's strict-mode type checker, for checking `keyboard.luau`, `options.luau` and `host.luau` when packing and in the desktop app. Host only.

It loads `glassdeck_luau_check` at run time with `libloading`. That library is the C++ wrapper over Luau's Analysis library in [`native/glassdeck-luau-check-native`](../../native).

## API

- `LuauChecker::load(dir)`: loads `glassdeck_luau_check.dll` (or `.so`) from `dir`.
- `LuauChecker::check(definitions, script)`: one problem per line, as `name:line:column: message`. Empty when the script checks clean.
- `check_options(source, options)`: an `options.luau` against the option builders.
- `check_host(source, options, messages)`: a `host.luau` against the host API, with `messages` the `host-bridge.d.luau` glassdeck-messages generates from `bridge.luau`.
  - In both, `options` is the `options.d.luau` that glassdeck-luau generates from the options.
- `LuauFile { name, text }`: a definition file or the script to check.
- `definitions::{COMMON, HOST, OPTIONS_API, CANVAS, KEYBOARD, CURRENT}`: the script APIs, included from `crates/glassdeck-luau/types/`.

The status codes mirror `GdLuauStatus` in `glassdeck_luau_check.h`.

## Platform

Needs the native library from `native/build.py`; see [native/README.md](../../native/README.md). No tests of its own: `glassdeck pack` exercises it.
