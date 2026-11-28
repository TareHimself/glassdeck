# glassdeck-host

A widget's or background's host half. `HostSide::start(sources, services)` runs `options.luau`, then `host.luau` in its own VM. With options but no `host.luau`, an implicit host forwards the options as they are, to the keyboard's `bridge.on.options`. glassdeckd runs one per package of the active theme; the desktop app runs one for the editor's preview.

## API

| Item | What it is |
|---|---|
| `Sources` | What a host side is made from: the two scripts, `bridge.luau`, the resource reader, the `network` hosts from `deck.json`, the message limit, `print` and the `net` wake callback |
| `Services` | What every host side on a thread shares: system readings, now playing and the audio tap |
| `apply_options(chosen, changed)` | `script.on_options`, or the implicit host's forward |
| `set_keyboard(connected)` | The keyboard came or went: `keyboard.connected()` follows it and `script.on_keyboard` hears each change |
| `take_options()` | The implicit host's options since the last call, latest only |
| `take_messages()`, `deliver_message(message, payload)` | `bridge` messages to and from the keyboard half |
| `take_live()` | Live resources made since the last call |
| `run_timers(now)`, `next_timer()` | `every` callbacks |
| `run_answers()` | Resumes callbacks whose `net` answers have arrived |

## Luau APIs

`host.luau` gets the shared ones from glassdeck-luau (`script.on_options`, `bridge.on.<msg>`, `color`, `json`, `every`, `any`) and these host-only ones:

| Global | Module | What it does |
|---|---|---|
| `keyboard` | | `keyboard.bridge.<msg>(value)` sends to the keyboard half; `keyboard.resource(name)`; `keyboard.connected()` |
| `script` | | `script.on_keyboard(fn(connected))`: the keyboard came or went. Not called at start; a script starts as if it's there |
| `host` | | `host.resource(name)`: a resource that stays on the host |
| `live`, `image` | `api` | Live resources for the keyboard half; image fitting so the keyboard only decodes small PNGs |
| `media` | `media` | Now playing, from Windows' media controls |
| `audio` | `audio` | `audio.bands(n)`: loudness per band of what the speakers play (WASAPI loopback, FFT). Capture starts on first use |
| `net` | `net` | `get`, `request`, `all`: HTTPS (plain HTTP to localhost only) to the hosts the package lists under `"network"`. The calling callback waits for the answer |
| `system` | | `cpu_percent`, `memory_percent` |

Types: `crates/glassdeck-luau/types/host.d.luau`.
