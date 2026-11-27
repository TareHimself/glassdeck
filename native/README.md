# native

C++ wrappers that give Rust a small C API over Slang and Luau's type checker, the language servers the desktop app's editors run, and the silo recipes that build them. Host only: the CLI uses them when packing, and the desktop app for checks, canvas export, previews and its editors. Nothing here runs on the keyboard.

## Build

Needs `uv` (Python 3.13+). silo comes from git, pinned in `pyproject.toml`.

```sh
cd native
uv run python build.py
```

`build.py` runs `silo build` on every folder with a `silopkg.py`, writes reports to `build-report/<module>/`, and copies each module's `bin/` into `target/native`.

## Where programs look

| Program | Looks in |
|---|---|
| `glassdeck` CLI | `GLASSDECK_NATIVE`, else beside `glassdeck.exe` |
| desktop app | `GLASSDECK_NATIVE`, else `../native` from the exe (so a dev build in `target/debug` finds `target/native`), else beside the exe |

## Modules

| Folder | Builds | Used by |
|---|---|---|
| `glassdeck-slang-native` | `glassdeck_slang` (shared): Slang's compilation API | `crates/glassdeck-slang` |
| `glassdeck-luau-check-native` | `glassdeck_luau_check` (shared): Luau's Analysis library (strict-mode type check) | `crates/glassdeck-luau-check` |
| `glassdeck-language-servers` | `luau-lsp` and `slangd`, copied from their recipes | the desktop app's editors (`apps/desktop/src-tauri/src/lsp.rs`) |

The two libraries are plain C exports, loaded at run time with `libloading`. Each header documents its API, and its status enum is mirrored by `Status` on the Rust side.

- `glassdeck_slang.h`: `gd_slang_reflect` (structs and functions carrying user attributes, as JSON), `gd_slang_compile` (an entry point to SPIR-V or GLSL), `gd_slang_has_profile`, and `gd_text_*` for returned text.
- `glassdeck_luau_check.h`: `gd_luau_check` (a script against definition files, one problem per line as `name:line:column: message`), and `gd_luau_text_*`.

Each wrapper is a CMake project (`CMakeLists.txt`, `CMakePresets.json`) built by its `silopkg.py`, with `cmake@4.3` as a tool.

## Recipes

`recipes/` holds the dependencies, found through the `glassdeck` registry in `silo.config.toml`:

| Recipe | How |
|---|---|
| `slang/2025.9.2` | Upstream's prebuilt release archive, checked by sha256. Nothing is compiled. Windows, Linux and macOS, x86_64 and arm64. Also provides `slangd` |
| `luau/0.748` | Built from source at a pinned commit, for its Analysis library and what it links. The same Luau release `mlua` runs |
| `luau-lsp/1.70.1` | Upstream's prebuilt release archive, checked by sha256. Nothing is compiled |

`silo.config.toml` also adds the `silo-recipes` registry, pinned to a commit, for recipes not kept in `recipes/`.

## Test

There are no tests here. The glassdeck-slang and glassdeck-luau-check tests do not load these libraries; `glassdeck pack` exercises both. The desktop app's `lsp.rs` tests run `luau-lsp` and `slangd` from `target/native`, and skip when they aren't built.
