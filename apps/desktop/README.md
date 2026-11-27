# Glassdeck desktop app

The app that edits widgets and backgrounds and applies a setup to the keyboard. It is a client of glassdeckd: it never talks to the keyboard itself.

- `src-tauri/`: the Rust side (crate `glassdeck-desktop`). Every command is listed in `build.rs` and allowed in `capabilities/default.json`. It:
  - owns the library, one source folder per package at `%APPDATA%\Glassdeck\library` (`library.rs`), and each package's still in `%APPDATA%\Glassdeck\thumbnails` (`stills.rs`)
  - reads `api.json`, calls glassdeckd's local API and relays `/events` to the window (`glassdeckd.rs`)
  - runs both halves of a package for the editor's previews, through glassdeck-widget, glassdeck-host and glassdeck-gpu (`preview.rs`, `canvas_preview.rs`, `layout_preview.rs`)
  - relays the editors' language servers over stdio (`lsp.rs`)
  - turns dropped videos and images into backgrounds (`videos.rs`, `images.rs`)
  - closes to the tray: the window hides and previews stop drawing until it's shown again (`tray.rs`)
- `src/`: React + TypeScript. Styles are CSS Modules: shared building blocks in `src/components/`, each with its `.module.css`, and screen-only layout beside each screen. `src/bindings.ts` is generated: commands, events and the `glassdeck-api` types, through tauri-specta.

## Run

Needs Node 22+, pnpm, a running glassdeckd, and the native libraries and language servers in `target/native` for checks, canvas previews and the editors ([native/README.md](../../native/README.md)).

```sh
pnpm install
pnpm tauri dev
```

A debug run rewrites `src/bindings.ts`. Commit it with the Rust change that caused it; `cargo test -p glassdeck-desktop` fails while it is stale.

## Test in a browser

`pnpm dev:mock` serves the frontend at `http://localhost:1421` with an in-memory backend (`src/mock/`) in place of Tauri: a sample library, a fake glassdeckd and generated preview frames. No glassdeckd, keyboard or native libraries needed.

```sh
pnpm test:e2e
```

Runs the Playwright tests in `e2e/` against that mode, in the installed Edge. A test can change the fake keyboard with `window.glassdeckMock.setKeyboard(...)`. A command the frontend calls with no handler in `src/mock/backend.ts` throws, so add one when adding a command.

## Build

```sh
pnpm tauri build
```

Makes an NSIS installer under `target/release/bundle/nsis/`.
