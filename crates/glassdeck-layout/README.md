# glassdeck-layout

Where every key and the widget area sit on the Flux keyboard's 1920x1080 panel, in panel pixels. Used on both sides.

Geometry adapted from Keyglass (see `NOTICE`).

## API

- `PANEL_WIDTH`, `PANEL_HEIGHT`
- `Variant`: `Ansi` or `Iso`.
- `Layout::new(variant)`:
  - `keys: Vec<Rect>`, indexed like the firmware's pressed-key bitmap (row by row, left to right).
  - `widget_area: Rect`, the band above the F-row where widgets go.
  - `key_at(x, y)`: the key under a panel point.
- `Rect`: `x`, `y`, `width`, `height`, `centre()`.

## Used by

- glassdeck-luau (`profile`: the widget area's size)
- glassdeck-receiver (key labels, keymap, Windows preview window)

## Platform

Portable, no dependencies.

## Test

```sh
cargo test -p glassdeck-layout
```

No ignored tests.
