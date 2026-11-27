# glassdeck-messages

A package's `bridge.luau`: the messages between its keyboard half (`keyboard.luau`) and its host half (`host.luau`), declared as Luau types. Used on both sides, with no Luau library, so the keyboard can read a package's messages too.

```lua
type Track = { title: string, artist: string }
export type nowPlaying = KeyboardState<Track>
export type tapped = HostEvent<{ key: number }>
```

Exported types are messages, local types are named shapes. A message's number is its place in the file.

## API

| Item | What it is |
|---|---|
| `parse(source)` | `bridge.luau` to `Messages`, or every `Problem` with its line and column |
| `Messages` | The messages in file order; `get(name)` gives a message's number and `Message` |
| `Message` | `name`, `kind`, `shape` |
| `Kind` | `KeyboardState` (to the keyboard, latest wins), `KeyboardEvent` (to the keyboard, every one in order), `HostEvent` (to the host, every one in order) |
| `Shape` | What a message carries, with named shapes resolved |
| `encode(shape, value)`, `decode(shape, bytes)` | A `Value` on the wire, untagged: flag bits, then values in field order |
| `definitions(messages, half, implicit_options)` | Luau types for one `Half`: `bridge.on.<msg>` it hears and `host.bridge.<msg>` or `keyboard.bridge.<msg>` it sends |
| `wrapper_definitions()` | The `KeyboardState`, `KeyboardEvent` and `HostEvent` types, for checking `bridge.luau` itself |
| `OPTIONS` | The keyboard half's built-in `options` message from the implicit host |

## Test

`tests/luau.rs` checks the generated types with Luau's checker from `target/native`, and skips when it isn't built ([native/README.md](../../native/README.md)).
