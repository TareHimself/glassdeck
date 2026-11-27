//! Luau type definitions from a package's messages: for writing `messages.luau` itself, and for each
//! half's script, what it hears and what it can send to the other half.

use crate::{Messages, Shape};

/// A package's two halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    /// `keyboard.luau`, on the keyboard
    Keyboard,
    /// `host.luau`, on the PC
    Host,
}

/// The kinds `messages.luau` is written with, so the language server knows them and explains them on hover.
pub fn wrapper_definitions() -> &'static str {
    r"-- messages.luau: one line per message, `export type name = Kind<shape>`. The name is the message's;
-- local `type` lines name shapes to reuse. A message carries numbers, booleans, strings, Color, vector,
-- lists `{ T }`, tables of those, and `?` for optional.

-- To the keyboard. State: only the latest matters, so a newer value replaces one not yet sent.
export type KeyboardState<T> = T
-- To the keyboard. Event: every one arrives, in order, none dropped.
export type KeyboardEvent<T> = T
-- To the host. Event: every one arrives, in order, none dropped.
export type HostEvent<T> = T
-- A colour, as four numbers from 0 to 1
export type Color = vector
"
}

/// The Luau type of a shape. A `Color` is written as `vector`, which is what a script holds.
fn luau(shape: &Shape) -> String {
    match shape {
        Shape::Number => "number".into(),
        Shape::Boolean => "boolean".into(),
        Shape::String => "string".into(),
        Shape::Color | Shape::Vector => "vector".into(),
        Shape::List(item) => format!("{{ {} }}", luau(item)),
        Shape::Optional(inner) => format!("{}?", luau(inner)),
        Shape::Record(fields) if fields.is_empty() => "{}".into(),
        Shape::Record(fields) => format!("{{ {} }}", fields.iter().map(|(name, shape)| format!("{name}: {}", luau(shape))).collect::<Vec<_>>().join(", ")),
    }
}

/// What `half`'s script gets from `messages.luau`: `MessagesHeard`, the callbacks it can register for
/// the messages that come to it, and `MessagesSent`, the calls that send to the other half.
/// `implicit_options`: `host.luau` is blank, so the keyboard half hears the built-in `options`, typed
/// by the generated `Options`.
pub fn definitions(messages: &Messages, half: Half, implicit_options: bool) -> String {
    let mut heard: Vec<String> =
        messages.0.iter().filter(|m| m.kind.to() == half).map(|m| format!("    {}: (callback: (value: {}) -> ()) -> (),\n", m.name, luau(&m.shape))).collect();
    if implicit_options && half == Half::Keyboard {
        heard.push(format!("    {}: (callback: (options: Options) -> ()) -> (),\n", crate::OPTIONS));
    }
    let sent: Vec<String> = messages.0.iter().filter(|m| m.kind.to() != half).map(|m| format!("    {}: (value: {}) -> (),\n", m.name, luau(&m.shape))).collect();
    format!(
        "-- From messages.luau. Made by Glassdeck; edits here are lost.\n\n-- What this half hears: script.on.<message>(callback)\nexport type MessagesHeard = {{\n{}}}\n\n-- What this half sends to the other: {}.script.<message>(value)\nexport type MessagesSent = {{\n{}}}\n",
        heard.concat(),
        match half {
            Half::Keyboard => "host",
            Half::Host => "keyboard",
        },
        sent.concat()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[test]
    fn each_half_hears_what_comes_to_it_and_sends_the_rest() {
        let messages = parse("export type palette = KeyboardState<{ colours: { Color }, speed: number }>\nexport type tapped = HostEvent<{ key: number, label: string? }>\n").unwrap();
        let keyboard = definitions(&messages, Half::Keyboard, false);
        assert!(keyboard.contains("palette: (callback: (value: { colours: { vector }, speed: number }) -> ()) -> (),"), "{keyboard}");
        assert!(keyboard.contains("tapped: (value: { key: number, label: string? }) -> (),"), "{keyboard}");
        let host = definitions(&messages, Half::Host, false);
        assert!(host.contains("tapped: (callback: (value: { key: number, label: string? }) -> ()) -> (),"), "{host}");
        assert!(host.contains("palette: (value: { colours: { vector }, speed: number }) -> (),"), "{host}");
    }
}
