# native

C++ wrappers that give Rust a small C API over Slang and Luau's type checker, and the silo recipes that build them. Host only: both are used when packing, never on the keyboard.

## Build

Needs `uv` (Python 3.13+). silo comes from git, pinned in `pyproject.toml`.

```sh
cd native
uv run python build.py
```

`build.py` runs `silo build` on every folder with a `silopkg.py`, writes reports to `build-report/<module>/`, and copies each module's `bin/` into `target/native`. The glassdeck CLI looks for them in `GLASSDECK_NATIVE`, else beside `glassdeck.exe`.

## Wrappers

| Folder | Library | Wraps | Rust side |
|---|---|---|---|
| `glassdeck-slang-native` | `glassdeck_slang` (shared) | Slang's compilation API | `crates/glassdeck-slang` |
| `glassdeck-luau-check-native` | `glassdeck_luau_check` (shared) | Luau's Analysis library (strict-mode type check) | `crates/glassdeck-luau-check` |

Both are plain C exports, loaded at run time with `libloading`. Each header documents its API, and its status enum is mirrored by `Status` on the Rust side.

- `glassdeck_slang.h`: `gd_slang_reflect` (structs and functions carrying user attributes, as JSON), `gd_slang_compile` (an entry point to SPIR-V or GLSL), `gd_slang_has_profile`, and `gd_text_*` for returned text.
- `glassdeck_luau_check.h`: `gd_luau_check` (a script against definition files, one problem per line as `name:line:column: message`), and `gd_luau_text_*`.

Each wrapper is a CMake project (`CMakeLists.txt`, `CMakePresets.json`) built by its `silopkg.py`, with `cmake@4.3` as a tool.

## Recipes

`recipes/` holds the dependencies, found through the `glassdeck` registry in `silo.config.toml`:

| Recipe | How |
|---|---|
| `slang/2025.9.2` | Upstream's prebuilt release archive, checked by sha256. Nothing is compiled. Windows, Linux and macOS, x86_64 and arm64 |
| `luau/0.748` | Built from source at a pinned commit, for its Analysis library and what it links. The same Luau release `mlua` runs |

`silo.config.toml` also adds the `silo-recipes` registry, pinned to a commit, for recipes not kept in `recipes/`.

## Test

There are no native tests, and the glassdeck-slang and glassdeck-luau-check tests do not load these libraries. `glassdeck pack` exercises both.
