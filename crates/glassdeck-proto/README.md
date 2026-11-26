# glassdeck-proto

The two wire protocols. Used on both sides.

| Protocol | Between | Transport |
|---|---|---|
| Link (crate root) | glassdeckd (host) and glassdeck-receiver (keyboard) | TCP |
| Compositor socket (`compositor`) | glassdeck-receiver and its renderers (glassdeck-gpu, glassdeck-local-web) | `SOCK_SEQPACKET`, dma-buf fds as `SCM_RIGHTS` |

No dependencies.

## Link

Every message is an 8-byte header (`VERSION`, kind, length) and a body of at most `MAX_BODY_LEN`. A build refuses any other `VERSION`.

- `Message`: the enum of every message, with `encode()` and `Message::read_from(reader)`.
  - Streams: `StreamStart(StreamInfo)`, `Frame(Frame)`, `StreamEnd`. A later `StreamStart` switches source.
  - Clock sync: `ClockProbe`, `ClockReply`.
  - Backgrounds: `SelectBackground`, `BackgroundData` (MessagePack from a canvas's host half).
  - Modules: `ModuleData`, `ModuleResource` (per widget instance).
  - Files: `PutFile` pieces to a `FileTarget` (background, module or profile), answered by `PutDone` or `PutRefused`.
- `StreamInfo`: `Codec` (H.264 or HEVC), `Mode` (`Live` with a clock offset, or `Paced`), size.
- `SessionState` and `step(state, message)`: the receiver's state machine, returning the `Action` to take.
- Constants: `BUILT_IN_BACKGROUND`, `LARGE_FILE_BYTES`, `PACED_SEND_AHEAD_US`, `PACED_PLAYOUT_DELAY_US`.
- `unix_micros()`: wall clock, for frame times and clock sync.

## Compositor socket

- `ToCompositor`: `Register(BufferLayout)` with the buffer's fd, `Present(BufferId)`.
- `FromCompositor`: `Registered`, `Refused`, `Released`, `Data` (a canvas's host data, at most `MAX_DATA_LEN`).

The C renderer glassdeck-local-web speaks the same format by hand.

## Used by

glassdeckd, glassdeck-capture, glassdeck-convert, glassdeck-package, glassdeck-wallpaper-engine, glassdeck-receiver, glassdeck-gpu (Linux only).

## Test

```sh
cargo test -p glassdeck-proto
```

Round trips for every message, refusal of bad versions, kinds, lengths and file targets, and the session table. No ignored tests.
