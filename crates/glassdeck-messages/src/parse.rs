//! Reads the subset of Luau type syntax `bridge.luau` is written in. Every problem is kept, each at
//! its line and column, so the editor can mark them all at once.

use std::collections::HashMap;

use crate::{Kind, Message, Messages, Problem, Shape};

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Name(String),
    Symbol(char),
    End,
}

#[derive(Clone, Debug)]
struct Spanned {
    token: Token,
    line: u32,
    column: u32,
}

/// A type as written, before named shapes are resolved.
#[derive(Clone, Debug)]
enum Written {
    Plain(Shape),
    Named { name: String, line: u32, column: u32 },
    List(Box<Written>),
    Record(Vec<(String, Written)>),
    Optional(Box<Written>),
}

fn tokens(source: &str) -> Result<Vec<Spanned>, Problem> {
    let chars: Vec<char> = source.chars().collect();
    let (mut i, mut line, mut column) = (0, 1u32, 1u32);
    let mut out = Vec::new();
    let advance = |i: &mut usize, line: &mut u32, column: &mut u32| {
        if chars[*i] == '\n' {
            *line += 1;
            *column = 1;
        } else {
            *column += 1;
        }
        *i += 1;
    };
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            advance(&mut i, &mut line, &mut column);
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            let block = chars.get(i + 2) == Some(&'[') && chars.get(i + 3) == Some(&'[');
            let (start_line, start_column) = (line, column);
            while i < chars.len() && (block || chars[i] != '\n') {
                if block && chars[i] == ']' && chars.get(i + 1) == Some(&']') {
                    advance(&mut i, &mut line, &mut column);
                    advance(&mut i, &mut line, &mut column);
                    break;
                }
                advance(&mut i, &mut line, &mut column);
                if block && i >= chars.len() {
                    return Err(Problem { line: start_line, column: start_column, message: "this comment is never closed with ]]".into() });
                }
            }
        } else if c.is_ascii_alphabetic() || c == '_' {
            let (start_line, start_column) = (line, column);
            let mut name = String::new();
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                name.push(chars[i]);
                advance(&mut i, &mut line, &mut column);
            }
            out.push(Spanned { token: Token::Name(name), line: start_line, column: start_column });
        } else {
            out.push(Spanned { token: Token::Symbol(c), line, column });
            advance(&mut i, &mut line, &mut column);
        }
    }
    out.push(Spanned { token: Token::End, line, column });
    Ok(out)
}

struct Parser {
    tokens: Vec<Spanned>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> &Spanned {
        &self.tokens[self.at]
    }

    fn next(&mut self) -> Spanned {
        let token = self.tokens[self.at].clone();
        if token.token != Token::End {
            self.at += 1;
        }
        token
    }

    fn problem(at: &Spanned, message: impl Into<String>) -> Problem {
        Problem { line: at.line, column: at.column, message: message.into() }
    }

    fn name(&mut self, what: &str) -> Result<(String, Spanned), Problem> {
        let at = self.next();
        match &at.token {
            Token::Name(name) => Ok((name.clone(), at)),
            _ => Err(Self::problem(&at, format!("expected {what}"))),
        }
    }

    fn symbol(&mut self, symbol: char) -> Result<(), Problem> {
        let at = self.next();
        if at.token == Token::Symbol(symbol) { Ok(()) } else { Err(Self::problem(&at, format!("expected '{symbol}'"))) }
    }

    fn is(&self, symbol: char) -> bool {
        self.peek().token == Token::Symbol(symbol)
    }

    /// Skips to the start of the next `type` or `export` line after a problem.
    fn recover(&mut self) {
        while !matches!(&self.peek().token, Token::End) {
            if let Token::Name(word) = &self.peek().token
                && (word == "type" || word == "export")
            {
                return;
            }
            self.next();
        }
    }

    fn written(&mut self) -> Result<Written, Problem> {
        let base = if self.is('{') {
            self.next();
            if self.is('}') {
                self.next();
                Written::Record(Vec::new())
            } else if matches!(&self.peek().token, Token::Name(_)) && self.tokens[self.at + 1].token == Token::Symbol(':') {
                self.record()?
            } else {
                let item = self.written()?;
                self.symbol('}')?;
                Written::List(Box::new(item))
            }
        } else if self.is('(') {
            return Err(Self::problem(self.peek(), "a message can't carry a function"));
        } else {
            let (name, at) = self.name("a type")?;
            match name.as_str() {
                "number" => Written::Plain(Shape::Number),
                "boolean" => Written::Plain(Shape::Boolean),
                "string" => Written::Plain(Shape::String),
                "Color" => Written::Plain(Shape::Color),
                "vector" => Written::Plain(Shape::Vector),
                _ => Written::Named { name, line: at.line, column: at.column },
            }
        };
        if self.is('|') || self.is('&') {
            return Err(Self::problem(self.peek(), "a message has one shape: no | or &"));
        }
        if self.is('?') {
            let at = self.next();
            if self.is('?') {
                return Err(Self::problem(&at, "one '?' is enough"));
            }
            return Ok(Written::Optional(Box::new(base)));
        }
        Ok(base)
    }

    /// `name: type` fields after the opening brace, through the closing one.
    fn record(&mut self) -> Result<Written, Problem> {
        let mut fields: Vec<(String, Written)> = Vec::new();
        loop {
            let (field, at) = self.name("a field name")?;
            if fields.iter().any(|(f, _)| *f == field) {
                return Err(Self::problem(&at, format!("{field} is already a field here")));
            }
            self.symbol(':')?;
            fields.push((field, self.written()?));
            if self.is(',') || self.is(';') {
                self.next();
            }
            if self.is('}') {
                self.next();
                return Ok(Written::Record(fields));
            }
        }
    }
}

enum Item {
    Message { name: String, kind: Kind, written: Written, at: Spanned },
    Shape { name: String, written: Written, at: Spanned },
}

fn item(p: &mut Parser) -> Result<Item, Problem> {
    let start = p.next();
    let exported = match &start.token {
        Token::Name(word) if word == "export" => {
            let (word, at) = p.name("type")?;
            if word != "type" {
                return Err(Parser::problem(&at, "expected type"));
            }
            true
        }
        Token::Name(word) if word == "type" => false,
        _ => return Err(Parser::problem(&start, "only `type` and `export type` lines belong in bridge.luau")),
    };
    let (name, at) = p.name("a name")?;
    if p.is('<') {
        return Err(Parser::problem(p.peek(), "a named shape can't take type parameters"));
    }
    p.symbol('=')?;
    if !exported {
        return Ok(Item::Shape { name, written: p.written()?, at });
    }
    let (wrapper, wrapper_at) = p.name("KeyboardState<...>, KeyboardEvent<...> or HostEvent<...>")?;
    let kind = Kind::ALL
        .into_iter()
        .find(|k| k.wrapper() == wrapper)
        .ok_or_else(|| Parser::problem(&wrapper_at, format!("{wrapper} isn't a kind of message: use KeyboardState, KeyboardEvent or HostEvent")))?;
    p.symbol('<')?;
    let written = p.written()?;
    p.symbol('>')?;
    Ok(Item::Message { name, kind, written, at })
}

struct Resolver<'a> {
    shapes: &'a HashMap<String, Written>,
    problems: Vec<Problem>,
}

impl Resolver<'_> {
    fn resolve(&mut self, written: &Written, seen: &mut Vec<String>) -> Option<Shape> {
        Some(match written {
            Written::Plain(shape) => shape.clone(),
            Written::List(item) => Shape::List(Box::new(self.resolve(item, seen)?)),
            Written::Optional(inner) => Shape::Optional(Box::new(self.resolve(inner, seen)?)),
            Written::Record(fields) => Shape::Record(fields.iter().map(|(name, w)| Some((name.clone(), self.resolve(w, seen)?))).collect::<Option<_>>()?),
            Written::Named { name, line, column } => {
                let problem = |message: String| Problem { line: *line, column: *column, message };
                let Some(named) = self.shapes.get(name) else {
                    self.problems.push(problem(format!(
                        "{name} isn't a type messages can carry: use number, boolean, string, Color, vector, {{ ... }} or a type declared here"
                    )));
                    return None;
                };
                if seen.contains(name) {
                    self.problems.push(problem(format!("{name} contains itself")));
                    return None;
                }
                seen.push(name.clone());
                let shape = self.resolve(named, seen);
                seen.pop();
                shape?
            }
        })
    }
}

/// Reads `bridge.luau`: its messages in order, or every problem found.
pub fn parse(source: &str) -> Result<Messages, Vec<Problem>> {
    let mut p = Parser { tokens: tokens(source).map_err(|p| vec![p])?, at: 0 };
    let mut problems = Vec::new();
    let mut items = Vec::new();
    while p.peek().token != Token::End {
        match item(&mut p) {
            Ok(item) => items.push(item),
            Err(problem) => {
                problems.push(problem);
                p.recover();
            }
        }
    }

    let mut shapes = HashMap::new();
    for item in &items {
        if let Item::Shape { name, written, at } = item
            && shapes.insert(name.clone(), written.clone()).is_some()
        {
            problems.push(Parser::problem(at, format!("{name} is already declared")));
        }
    }
    let mut resolver = Resolver { shapes: &shapes, problems: Vec::new() };
    let mut messages: Vec<Message> = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    for item in &items {
        if let Item::Message { name, kind, written, at } = item {
            if names.contains(&name.as_str()) {
                problems.push(Parser::problem(at, format!("{name} is already a message")));
                continue;
            }
            if name == crate::OPTIONS {
                problems.push(Parser::problem(at, "options is the message the options arrive in when host.luau is blank; name yours something else"));
                continue;
            }
            names.push(name);
            if let Some(shape) = resolver.resolve(written, &mut Vec::new()) {
                messages.push(Message { name: name.clone(), kind: *kind, shape });
            }
        }
    }
    for item in &items {
        if let Item::Shape { name, written, .. } = item {
            resolver.resolve(written, &mut vec![name.clone()]);
        }
    }
    problems.extend(resolver.problems);
    problems.sort_by_key(|p| (p.line, p.column));
    problems.dedup();
    if problems.is_empty() { Ok(Messages(messages)) } else { Err(problems) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(fields: &[(&str, Shape)]) -> Shape {
        Shape::Record(fields.iter().map(|(n, s)| ((*n).to_owned(), s.clone())).collect())
    }

    #[test]
    fn messages_come_in_order_with_named_shapes_resolved() {
        let source = r#"
-- the track playing
type Track = { title: string, artist: string, length: number? }

export type nowPlaying = KeyboardState<Track>
export type palette = KeyboardState<{ colours: { Color }, speed: number }>
export type trackChanged = KeyboardEvent<Track>
export type tapped = HostEvent<{ key: number; held: boolean }>
export type skip = HostEvent<{}>
"#;
        let track = record(&[("title", Shape::String), ("artist", Shape::String), ("length", Shape::Optional(Box::new(Shape::Number)))]);
        let messages = parse(source).unwrap();
        let summary: Vec<(&str, Kind)> = messages.0.iter().map(|m| (m.name.as_str(), m.kind)).collect();
        assert_eq!(
            summary,
            [
                ("nowPlaying", Kind::KeyboardState),
                ("palette", Kind::KeyboardState),
                ("trackChanged", Kind::KeyboardEvent),
                ("tapped", Kind::HostEvent),
                ("skip", Kind::HostEvent)
            ]
        );
        assert_eq!(messages.0[0].shape, track);
        assert_eq!(messages.0[1].shape, record(&[("colours", Shape::List(Box::new(Shape::Color))), ("speed", Shape::Number)]));
        assert_eq!(messages.0[4].shape, record(&[]));
        assert_eq!(messages.get("tapped").map(|(n, _)| n), Some(3));
    }

    #[test]
    fn each_problem_is_at_its_line() {
        let source = "export type a = KeyboardState<{ when: Date }>\nexport type b = Signal<{}>\ntype T = { x: number, x: number }\nexport type a = HostEvent<{}>\nlocal x = 1\n";
        let problems = parse(source).unwrap_err();
        let at: Vec<(u32, &str)> = problems.iter().map(|p| (p.line, p.message.as_str())).collect();
        assert_eq!(at.len(), 5, "{problems:#?}");
        assert!(at[0].0 == 1 && at[0].1.starts_with("Date isn't a type"));
        assert!(at[1].0 == 2 && at[1].1.starts_with("Signal isn't a kind of message"));
        assert!(at[2].0 == 3 && at[2].1.contains("already a field"));
        assert!(at[3].0 == 4 && at[3].1.contains("already a message"));
        assert!(at[4].0 == 5 && at[4].1.starts_with("only `type`"));
    }

    #[test]
    fn a_shape_that_contains_itself_is_refused() {
        let problems = parse("type Node = { next: Node? }\nexport type n = KeyboardState<Node>\n").unwrap_err();
        assert!(problems.iter().any(|p| p.message == "Node contains itself"), "{problems:#?}");
    }

    #[test]
    fn functions_and_unions_have_no_place() {
        assert!(parse("export type f = HostEvent<(number) -> ()>").is_err());
        assert!(parse("export type u = HostEvent<number | string>").is_err());
    }

    #[test]
    fn options_is_kept_for_the_implicit_host() {
        let problems = parse("export type options = KeyboardState<{}>").unwrap_err();
        assert!(problems[0].message.starts_with("options is the message"));
    }

    #[test]
    fn comments_of_both_kinds_are_skipped() {
        let messages = parse("--[[ block\n comment ]] export type a = HostEvent<{}> -- line\n").unwrap();
        assert_eq!(messages.0.len(), 1);
    }
}
