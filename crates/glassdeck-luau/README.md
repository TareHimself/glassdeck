# glassdeck-luau

The Luau core both halves share: glassdeck-host runs host halves, and glassdeck-widget and glassdeck-gpu run keyboard halves. Used on both sides.

```mermaid
flowchart LR
  h[host.luau<br/>glassdeckd] -- "keyboard.bridge.x(), packed by position" --> L[Link] -- "bridge.on.x" --> k[keyboard.luau<br/>receiver / glassdeck-gpu]
```

## API

| Item | Role |
|---|---|
| `Vm` | One instance's VM: `new`, `install_print`, `seal` (read-only globals), `load`, `call`, `run_due_timers`, `next_timer`, `used_memory` |
| `Vm::every_function` | The `every(seconds, fn)` timer, for each host to expose |
| `CALL_LIMIT` | Any call into a script must return within 100 ms, or it is stopped |
| `encode` / `decode` | Plain Luau data to and from MessagePack, which the implicit host forwards options in |
| `messages` | `bridge`: `ScriptMessages::install` gives a half `bridge.on.<msg>` and `<other half>.bridge.<msg>`, checks each value against its shape and packs it; `take_sent`, `deliver`, `deliver_options` |
| `waiting` | Callbacks that can wait, as coroutines: `Vm::allow_waiting`, `Vm::run`, `Vm::resume_answered`, `Waits` (used by `net`) |
| `json` | `json.decode`, `json.encode` |
| `color` | `color.hex`, `color.rgb`, `color.hsv` |
| `options` | `options.luau`: `OptionsScript` (`first_pass`, `run`), `Panel`, `Item`, `OptionItem`, `Kind`, `OptionValue`, `Values`, `Resolved` |
| `Panel::definitions` | Generates the `options.d.luau` that types `current` and `on_options` for the checker |
| `placement` | Where widgets sit: `WidgetArea`, `Placement`, `PlacedWidget`, `GRID`, and `check_widgets` for the rules a layout must keep. An instance's number is its place in the list, on both sides |
| Resource handles | `keyboard.resource(name)`, `host.resource(name)` and `live.resource(name)` return an opaque handle: `ResourceRef`, `LiveName`, `resource_of`, `MAX_LIVE_RESOURCE_BYTES` |

`types/` holds the Luau type definitions for the script APIs (`common`, `host`, `keyboard`, `canvas`, `options-api`). glassdeck-luau-check includes them.

## Platform

Portable. `mlua` builds Luau from source, so no native libraries are needed.
