# Glassdeck desktop app

The app that edits widgets and backgrounds and applies a setup to the keyboard. It is a client of glassdeckd: it never talks to the keyboard itself.

- `src-tauri/`: the Rust side (crate `glassdeck-desktop`). It reads `api.json`, calls glassdeckd's local API and relays `/events` to the window. Every command is listed in `build.rs` and allowed in `capabilities/default.json`.
- `src/`: React + TypeScript. Styles are CSS Modules: shared building blocks in `src/components/`, each with its `.module.css`, and screen-only layout beside each screen. `src/bindings.ts` is generated: commands, events and the `glassdeck-api` types, through tauri-specta.

## Run

Needs Node 22+, pnpm and a running glassdeckd.

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
