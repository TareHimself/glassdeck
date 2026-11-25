use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::w;

pub fn steam_root() -> Result<PathBuf> {
    let mut buffer = [0u16; 1024];
    let mut bytes = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Valve\\Steam"),
            w!("SteamPath"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
        .ok()
        .context("Steam is not installed (no SteamPath in the registry)")?;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(PathBuf::from(String::from_utf16(&buffer[..len])?))
}

/// Every library folder Steam knows, read from `steamapps/libraryfolders.vdf`.
pub fn library_folders(steam_root: &Path) -> Result<Vec<PathBuf>> {
    let path = steam_root.join("steamapps").join("libraryfolders.vdf");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let root = parse_vdf(&text).with_context(|| format!("parsing {}", path.display()))?;
    let Some(Vdf::Map(folders)) = root.get("libraryfolders") else {
        bail!("{} has no libraryfolders section", path.display());
    };
    folders
        .iter()
        .map(|(key, folder)| match folder.get("path") {
            Some(Vdf::Str(path)) => Ok(PathBuf::from(path)),
            _ => bail!("library folder {key} has no path"),
        })
        .collect()
}

/// Valve's KeyValues text format: quoted keys, each followed by a quoted value or a braced block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Vdf {
    Str(String),
    Map(Vec<(String, Vdf)>),
}

impl Vdf {
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Self::Map(entries) => entries.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Self::Str(_) => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Str(String),
    Open,
    Close,
}

pub fn parse_vdf(text: &str) -> Result<Vdf> {
    let tokens = tokenize(text)?;
    let mut rest = tokens.as_slice();
    let entries = parse_entries(&mut rest)?;
    if !rest.is_empty() {
        bail!("unmatched closing brace");
    }
    Ok(Vdf::Map(entries))
}

fn parse_entries(tokens: &mut &[Token]) -> Result<Vec<(String, Vdf)>> {
    let mut entries = Vec::new();
    loop {
        match tokens.first() {
            None | Some(Token::Close) => return Ok(entries),
            Some(Token::Open) => bail!("a block without a key"),
            Some(Token::Str(key)) => {
                let key = key.clone();
                *tokens = &tokens[1..];
                let value = match tokens.first() {
                    Some(Token::Str(value)) => {
                        let value = Vdf::Str(value.clone());
                        *tokens = &tokens[1..];
                        value
                    }
                    Some(Token::Open) => {
                        *tokens = &tokens[1..];
                        let inner = parse_entries(tokens)?;
                        match tokens.first() {
                            Some(Token::Close) => *tokens = &tokens[1..],
                            _ => bail!("block {key:?} is not closed"),
                        }
                        Vdf::Map(inner)
                    }
                    _ => bail!("key {key:?} has no value"),
                };
                entries.push((key, value));
            }
        }
    }
}

fn tokenize(text: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '"' => {
                let mut s = String::new();
                loop {
                    match chars.next() {
                        None => bail!("unterminated string"),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => bail!("unterminated string"),
                        },
                        Some(other) => s.push(other),
                    }
                }
                tokens.push(Token::Str(s));
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {}
            other => bail!("unexpected {other:?}"),
        }
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARIES: &str = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"apps" { "228980" "190783083" }
	}
	// a comment
	"1"
	{
		"path"		"E:\\SteamLibrary"
	}
}
"#;

    #[test]
    fn reads_library_paths_with_escaped_backslashes() {
        let root = parse_vdf(LIBRARIES).unwrap();
        let folders = root.get("libraryfolders").unwrap();
        assert_eq!(folders.get("1").unwrap().get("path"), Some(&Vdf::Str(r"E:\SteamLibrary".into())));
        assert_eq!(folders.get("0").unwrap().get("apps").unwrap().get("228980"), Some(&Vdf::Str("190783083".into())));
    }

    #[test]
    fn unclosed_blocks_fail() {
        assert!(parse_vdf(r#""a" { "b" "c""#).is_err());
        assert!(parse_vdf(r#""a" }"#).is_err());
    }
}
