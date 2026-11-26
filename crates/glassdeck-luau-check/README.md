# glassdeck-luau-check

Luau's strict-mode type checker, for checking `keyboard.luau`, `options.luau` and `host.luau` when packing. Host only (it runs at pack time, not on the keyboard).

It loads `glassdeck_luau_check` at run time with `libloading`. That library is the C++ wrapper over Luau's Analysis library in [`native/glassdeck-luau-check-native`](../../native).

## API

- `LuauChecker::load(dir)`: loads `glassdeck_luau_check.dll` (or `.so`) from `dir`.
- `LuauChecker::check(definitions, script)`: one problem per line, as `name:line:column: message`. Empty when the script checks clean.
- `check_options(source, options)`: an `options.luau` against the option builders.
- `check_host(source, options)`: a `host.luau` against the host API.
  - In both, `options` is the `options.d.luau` that glassdeck-luau generates from the options.
- `LuauFile { name, text }`: a definition file or the script to check.
- `definitions::{COMMON, HOST, OPTIONS_API, CANVAS}`: the script APIs, included from `crates/glassdeck-luau/types/`.

The status codes mirror `GdLuauStatus` in `glassdeck_luau_check.h`.

## Used by

- glassdeck-slang (`pack_canvas`, `check_module`)
- glassdeck (the CLI's `pack`)

## Platform

Needs the native library, built by `native/build.py` into `target/native`. The CLI finds it through `GLASSDECK_NATIVE`, else beside `glassdeck.exe`.

## Test

No tests of its own. It is exercised through `glassdeck pack`.
