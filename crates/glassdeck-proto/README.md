# glassdeck-proto

The two wire protocols. Used on both sides.

| Protocol | Between | Transport |
|---|---|---|
| Link (crate root) | glassdeckd (host) and glassdeck-receiver (keyboard) | TCP |
| Compositor socket (`compositor`) | glassdeck-receiver and its renderers (glassdeck-gpu, glassdeck-local-web) | `SOCK_SEQPACKET`, dma-buf fds as `SCM_RIGHTS` |

## Link

Every message is an 8-byte header (`VERSION`, kind, length) and a body of at most `MAX_BODY_LEN`. A build refuses any other `VERSION` (now 14).

- `Message`: the enum of every message, with `encode()` and `Message::read_from(reader)`.
  - Streams: `StreamStart(StreamInfo)`, `Frame(Frame)`, `StreamEnd`. A later `StreamStart` switches source.
  - Clock sync: `ClockProbe`, `ClockReply`.
  - Backgrounds: `SelectBackground` (a package id), `BackgroundData` (the implicit host's options, MessagePack).
  - Widgets: `SetWidgets { widgets: Vec<WidgetSlot> }` (the layout; an instance's number is its place in it), then per instance `ModuleData` (the implicit host's options) and `ModuleResource` (a live resource).
  - Scripts: `ScriptMessage { to: ScriptTarget, kind: Kind, message, payload }`, a `bridge.luau` message either way. `ScriptTarget` is `Background` or `Widget(instance)`; `kind` and the payload's packing come from glassdeck-messages.
  - Packages: the keyboard sends `Inventory { packages }` (file name and digest) on every connect; the host uploads what it lacks with `PutFile` and deletes what the active theme no longer uses with `RemovePackages { names }`.
  - Files: `PutFile` pieces to a `FileTarget` (one variant, `Package(file name)`, a `<qualified id>.deck`), answered by `PutDone` or `PutRefused`.
- `StreamInfo`: `Codec` (H.264 or HEVC), `Mode` (`Live` with a clock offset, or `Paced`), size.
- `SessionState` and `step(state, message)`: the receiver's state machine, returning the `Action` to take.
- Constants: `BUILT_IN_BACKGROUND` (`background.core.glassdeck.gradient`), `LARGE_FILE_BYTES`, `PACED_SEND_AHEAD_US`, `PACED_PLAYOUT_DELAY_US`.
- `unix_micros()`: wall clock, for frame times and clock sync.

## Compositor socket

- `ToCompositor`: `Register(BufferLayout)` with the buffer's fd, `Present(BufferId)`, `ScriptMessage` (a `HostEvent` from the canvas's script to its host half).
- `FromCompositor`: `Registered`, `Refused`, `Released`, `Data` (the implicit host's options, at most `MAX_DATA_LEN`), `ScriptMessage` (a message from the host half, at most `MAX_SCRIPT_PAYLOAD_LEN`).

The C renderer glassdeck-local-web speaks the same format by hand.
