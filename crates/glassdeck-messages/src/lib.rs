//! A package's `messages.luau`: the messages between its two halves, as Luau types, one per line:
//!
//! ```lua
//! type Track = { title: string, artist: string }
//! export type nowPlaying = KeyboardState<Track>
//! export type tapped = HostEvent<{ key: number }>
//! ```
//!
//! It's ordinary Luau, so the language server checks it, but only this subset is read: exported types
//! are messages, local types are named shapes. Parsing it here, with no Luau library, lets the
//! keyboard read a package's messages too. Order matters: a message's number is its place in the file.

mod codec;
mod definitions;
mod parse;

pub use codec::{Malformed, Value, decode, encode};
pub use definitions::{Half, definitions, wrapper_definitions};
pub use parse::parse;

/// The keyboard half's built-in message: the options as they are, from the implicit host (a blank
/// `host.luau`). Its shape comes from `options.luau`, so it isn't declared in `messages.luau`.
pub const OPTIONS: &str = "options";

/// How a message travels, by the half it goes to and whether only its latest value matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// To the keyboard; only the latest matters, so a newer value replaces one not yet sent
    KeyboardState,
    /// To the keyboard; every one arrives, in order
    KeyboardEvent,
    /// To the host; every one arrives, in order
    HostEvent,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::KeyboardState, Kind::KeyboardEvent, Kind::HostEvent];

    /// Its wrapper type's name in `messages.luau`.
    pub fn wrapper(self) -> &'static str {
        match self {
            Kind::KeyboardState => "KeyboardState",
            Kind::KeyboardEvent => "KeyboardEvent",
            Kind::HostEvent => "HostEvent",
        }
    }

    /// The half that receives it.
    pub fn to(self) -> Half {
        match self {
            Kind::KeyboardState | Kind::KeyboardEvent => Half::Keyboard,
            Kind::HostEvent => Half::Host,
        }
    }
}

/// What a message carries, with every named shape resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Number,
    Boolean,
    String,
    /// `Color`: four numbers, as a Luau `vector`
    Color,
    Vector,
    List(Box<Shape>),
    /// Fields in the order they're written, which is the order they travel in
    Record(Vec<(String, Shape)>),
    Optional(Box<Shape>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub name: String,
    pub kind: Kind,
    pub shape: Shape,
}

/// A package's messages, in the order `messages.luau` declares them; a message's number is its index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Messages(pub Vec<Message>);

impl Messages {
    pub fn get(&self, name: &str) -> Option<(u16, &Message)> {
        self.0.iter().enumerate().find(|(_, m)| m.name == name).map(|(i, m)| (i as u16, m))
    }
}

/// A problem at a place in `messages.luau`, 1-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "messages.luau:{}:{}: {}", self.line, self.column, self.message)
    }
}
