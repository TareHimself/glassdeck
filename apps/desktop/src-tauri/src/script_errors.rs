//! Script errors as the editors show them: at their file and line, with the message alone, rather than
//! Luau's text with its chunk name and traceback.

use serde::Serialize;
use specta::Type;

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct ScriptError {
    /// The script it's in, such as `keyboard.luau`, when Luau named it
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
    /// It doesn't parse; the language server already marks these in the editor
    pub syntax: bool,
}

const CHUNK_START: &str = "[string \"";
const CHUNK_END: &str = "\"]:";

/// Finds `[string "file"]:line: message` in Luau's error text; the first line of what follows is the message.
pub fn located(text: &str) -> ScriptError {
    let syntax = text.contains("syntax error");
    let parsed = text.find(CHUNK_START).and_then(|start| {
        let rest = &text[start + CHUNK_START.len()..];
        let (file, rest) = rest.split_once(CHUNK_END)?;
        let (line, rest) = rest.split_once(':')?;
        let message = rest.lines().next().unwrap_or_default().trim();
        Some(ScriptError { file: Some(file.to_owned()), line: line.trim().parse().ok(), message: message.to_owned(), syntax })
    });
    parsed.unwrap_or_else(|| ScriptError { file: None, line: None, message: text.lines().next().unwrap_or_default().trim().to_owned(), syntax })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runtime_error_is_its_file_line_and_message_without_the_traceback() {
        let text = "running keyboard.luau: runtime error: [string \"keyboard.luau\"]:12: attempt to index nil with 'on_draw'\nstack traceback:\n\t[C]: in ?";
        assert_eq!(
            located(text),
            ScriptError { file: Some("keyboard.luau".into()), line: Some(12), message: "attempt to index nil with 'on_draw'".into(), syntax: false }
        );
    }

    #[test]
    fn a_syntax_error_says_so() {
        let error = located("running keyboard.luau: syntax error: [string \"keyboard.luau\"]:12: Incomplete statement: expected assignment or a function call");
        assert!(error.syntax);
        assert_eq!(error.line, Some(12));
    }

    #[test]
    fn text_without_a_location_is_kept_as_the_message() {
        assert_eq!(located("the widget drew nothing").message, "the widget drew nothing");
        assert_eq!(located("the widget drew nothing").line, None);
    }
}
