# glassdeck-slang

Slang's compilation API, and `glassdeck pack` for canvases. Host only: canvases are compiled when packing (by the CLI or the desktop app), so the keyboard never runs Slang.

It loads `glassdeck_slang` at run time with `libloading`. That library is the C++ wrapper in [`native/glassdeck-slang-native`](../../native).

## Packing a canvas

A canvas folder holds `deck.json`, `canvas.slang` and optionally `keyboard.luau`, `options.luau`, `host.luau` and `bridge.luau`. `pack::pack_canvas(slang, luau, folder, out)` turns it into a background `.deck`:

```mermaid
flowchart LR
  s[canvas.slang] --> r[reflect] --> shape[CanvasShape]
  shape --> glue[glue module per pass and canvas] --> c[compile to GLSL 450] --> e[essl::fragment_to_essl] --> deck[.deck]
  shape --> t[".glassdeck/ types"] --> chk[type-check scripts] --> deck
```

1. Reflect the shader (with `slang/glassdeck.slang`, the glassdeck Slang API) into a `CanvasShape`: the `[Canvas]` function, its `[Inputs]` struct, scale and frame rate, its `[Target]` textures (`[Feedback]`, `[Scale]`, `[Format]`) and its `[Pass(n)] [Writes("name")]` functions (`PassShape`), at most 4 of each.
2. Write `.glassdeck/` beside the sources: the API definitions plus generated `inputs.d.luau`, `options.d.luau` and the bridge types (`keyboard-bridge.d.luau`, `host-bridge.d.luau`), for editors and the checks.
3. Type-check each script with glassdeck-luau-check.
4. Generate a glue module (entry point `gd_fragment`, inputs uniform `gd_inputs`), compile to GLSL, and rewrite it as GLSL ES 3.00 for the keyboard's GLES 3 driver. Each pass gets its own glue and shader.
5. Write the manifest with the canvas's `details`, `shader.essl`, `pass{n}.essl` and the scripts (see glassdeck-package).

`pack::check_widget(luau, folder)` does steps 2 and 3 for a widget's `options.luau` and `host.luau`. The desktop app runs the same steps without packing, through `compile_canvas`, `canvas_problems` and `widget_problems`.

## API

| Item | Role |
|---|---|
| `Slang::load(dir)` | Loads the wrapper from `dir` |
| `Slang::reflect(sources)` | `Declaration`s carrying user attributes |
| `Slang::compile(sources, entry, target, profile)` | Code for `Target::Spirv` or `Target::Glsl` |
| `Slang::has_profile(name)` | Whether Slang knows a profile |
| `Source { name, path, text }` | A module as text; later modules may import earlier ones |
| `ShaderError`, `Status` | Slang's diagnostics; `Status` mirrors `GdStatus` in `glassdeck_slang.h` |
| `canvas::CanvasShape`, `canvas::PassShape` | Canvas shape from reflection, glue and input definitions |
| `essl::fragment_to_essl` | GLSL 450 to GLSL ES 3.00, fragment shaders only |
| `pack::{pack_canvas, check_widget, is_canvas_folder}` | The pack steps |
| `pack::{compile_canvas, canvas_problems, widget_problems, CompiledCanvas, ScriptCheck}` | Compiling and checking without packing, for the desktop app |
| `pack::write_message_types` | Writes each half's bridge types into `.glassdeck/` (`TYPES_FOLDER`) from unsaved `bridge.luau` text, for the language servers |

## Platform

Needs `glassdeck_slang` and Slang's own libraries from `native/build.py`; see [native/README.md](../../native/README.md). The tests cover `canvas` and `essl` and need no native library.
