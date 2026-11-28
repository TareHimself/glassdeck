# glassdeckd

The host daemon, and the one program that talks to the keyboard. It owns the Link, runs sources in worker processes, runs the host sides of the active theme's background and widgets, and serves the local HTTP API that the CLI and the desktop app use. Windows only.

```mermaid
flowchart LR
  clients[CLI, desktop app] -- HTTP + SSE --> api[api]
  api --> runner[runner]
  api --> host[module_host]
  api --> whep[whep] --> feeds[feeds]
  feeds -- preview workers --> w
  api --> active[active]
  runner -- spawns --> w[source workers]
  w -- localhost socket --> runner
  p[stream programs] -- stream protocol v1 --> attached[attached]
  attached --> runner
  attached --> whep
  runner --> link[link]
  host --> link
  link -- Link, TCP --> k[keyboard]
```

## Modules

| Module | What it does |
|---|---|
| `link` | The Link to the keyboard: a reconnecting state machine (`step`), plus `Uplink`, which queues streams, uploads, widget data, the background choice and the widget layout |
| `runner` | Runs at most one thing on the keyboard at a time: a source in a worker process, or an attached stream (`step` is the state table). `StatusBoard` holds its status for waiters and publishes every change |
| `worker` | The worker side, run as `glassdeckd source`, because CEF can start only once per process |
| `module_host` | Runs the host side of the theme's background and of each widget through [glassdeck-host](../glassdeck-host). Restarts only what changed |
| `theme` | The active theme: checked against the installed packages, kept in `theme.json`, and turned into the keyboard's widget layout |
| `active` | glassdeckd's copies of the active theme's packages and their keyboard copies, so it runs host sides and restores at login without the app |
| `feeds` | Who watches which source: the keyboard's own encoder feeds viewers of what it plays; any other watched source gets a preview worker until its last viewer leaves |
| `whep` | WebRTC previews (WHEP, over str0m): each session sends one source's encoded frames as RTP, with no re-encode |
| `api` | The axum server. Its routes and types live in [glassdeck-api](../glassdeck-api) |
| `error` | Maps failures to API error codes. A failure is marked where it happens (`bad_request`, `conflict`); unmarked ones are internal |
| `streams` | `StreamName`, the built-in monitor streams, and `Stream`: every stream a theme can show now |
| `attached` | Streams other programs attach over stream protocol v1 ([glassdeck-stream](../glassdeck-stream)). Each encodes only while watched (`step` is the state table); `VideoCheck` holds its video to the protocol |
| `config` | `Paths` and `config.json` |
| `system` | One instance per user, logging to a file, and starting at login |

## Files

| Path | What |
|---|---|
| `%APPDATA%\Glassdeck\config.json` | The keyboard's address and the Link's rate |
| `%APPDATA%\Glassdeck\theme.json` | The active theme, as the app last pushed it |
| `%LOCALAPPDATA%\Glassdeck\active\` | Copies of the active theme's packages only, with their keyboard copies in `keyboard\` and digests in `active.json` |
| `%LOCALAPPDATA%\Glassdeck\api.json` | The API's port, the stream port and the token |

## Run

```sh
glassdeckd run [--keyboard 10.0.0.186:7878] [--log]
glassdeckd install     # start at login
glassdeckd uninstall
```

## Test

`probe_serves_a_monitor_to_a_browser` (`whep`) is ignored: it needs a monitor, a GPU and a browser.

```sh
cargo test -p glassdeckd probe -- --ignored --nocapture
```
