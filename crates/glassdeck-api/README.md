# glassdeck-api

The types glassdeckd's local API sends and takes, shared by glassdeckd, the `glassdeck` CLI and the desktop app. It has no I/O beyond finding `api.json`.

## Reaching the API

glassdeckd listens on `127.0.0.1` at a random port and writes `%LOCALAPPDATA%\Glassdeck\api.json` (`ApiFile`) with the port and a token. Every request carries `Authorization: Bearer <token>`.

## Routes

| Route | Takes | Answers |
|---|---|---|
| `GET /status` | | `Status` |
| `GET /streams` | | `Streams` |
| `GET /theme` | | `Theme` |
| `PUT /theme` | `Theme` | `ThemeApplied` |
| `PUT /deploy` | `Deploy` | `Deployed` |
| `GET /events` | | server-sent `Event`s |

- **A `Theme` is one whole setup:** `format` 1, `background` (`{package, options}`), an optional `stream` shown whenever it's available, and `widgets`, each `{package, x, y, w, h, options}`. Packages are named by id.
- **`PUT /theme` makes a theme the active one,** and glassdeckd acts only on what changed. `ThemeApplied` says where its stream got to (`ShowState`, including `unavailable`).
- **Options values are JSON:** a number, a boolean or a string, with colours as `#rrggbbaa`. The panel types (`OptionsView`) are for the app, which runs `options.luau` itself.
- **`/events` first sends `Event::Status` with everything,** then one event per change: link, stream, theme, deployed. A client that falls behind gets `Event::Status` again.

## Errors

A refusal answers with `ApiError { code, error }` and the HTTP status its `ErrorCode` names.

| Code | HTTP | When |
|---|---|---|
| `unauthorized` | 401 | No token, or the wrong one |
| `bad_request` | 400 | The body or a parameter doesn't parse, or the value doesn't fit the option |
| `not_found` | 404 | No such route, stream, widget or option |
| `method_not_allowed` | 405 | The route exists, but not with that method |
| `conflict` | 409 | Busy with something that has to finish first |
| `unavailable` | 503 | The keyboard isn't connected |
| `internal` | 500 | Anything else |

## TypeScript

The types derive `ts_rs::TS`, so the frontend gets matching TypeScript types. Regenerate them with:

```sh
cargo test -p glassdeck-api
```

They're written to `bindings/`, which is committed so the app can import them.
