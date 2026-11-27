//! Luau's type checker, through `glassdeck_luau_check`, the small C++ wrapper over Luau's Analysis
//! library in `native/glassdeck-luau-check-native` that silo builds.

use std::ffi::{CString, c_char};
use std::path::Path;

use anyhow::{Context, Result, bail};

/// A Luau file as text: a definition file, or the script to check.
#[derive(Clone, Copy)]
pub struct LuauFile<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

#[repr(C)]
struct GdLuauFile {
    name: *const c_char,
    text: *const c_char,
}

#[repr(C)]
struct GdLuauText {
    _private: [u8; 0],
}

type Check = unsafe extern "C" fn(*const GdLuauFile, i32, *const GdLuauFile, *mut *mut GdLuauText) -> i32;
type TextData = unsafe extern "C" fn(*const GdLuauText) -> *const u8;
type TextSize = unsafe extern "C" fn(*const GdLuauText) -> usize;
type TextFree = unsafe extern "C" fn(*mut GdLuauText);

/// Mirrors `GdLuauStatus` in the wrapper's header.
const OK: i32 = 0;
const DEFINITIONS_FAILED: i32 = 2;

pub struct LuauChecker {
    check: Check,
    text_data: TextData,
    text_size: TextSize,
    text_free: TextFree,
    _library: libloading::Library,
}

impl LuauChecker {
    /// Loads `glassdeck_luau_check` from `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(libloading::library_filename("glassdeck_luau_check"));
        // SAFETY: loading the wrapper runs no initialisers beyond the C++ runtime's.
        let library = unsafe { libloading::Library::new(&path) }.with_context(|| format!("loading {}", path.display()))?;
        // SAFETY: the symbols are the wrapper's exports, declared with these signatures in its header.
        unsafe {
            Ok(Self {
                check: *library.get(b"gd_luau_check\0")?,
                text_data: *library.get(b"gd_luau_text_data\0")?,
                text_size: *library.get(b"gd_luau_text_size\0")?,
                text_free: *library.get(b"gd_luau_text_free\0")?,
                _library: library,
            })
        }
    }

    /// Type-checks `script` in strict mode against `definitions`; each problem is "name:line:column: message".
    pub fn check(&self, definitions: &[LuauFile], script: LuauFile) -> Result<Vec<String>> {
        let strings = definitions
            .iter()
            .chain([&script])
            .map(|f| Ok((CString::new(f.name)?, CString::new(f.text).with_context(|| format!("{} has a NUL in it", f.name))?)))
            .collect::<Result<Vec<_>>>()?;
        let files: Vec<GdLuauFile> = strings.iter().map(|(name, text)| GdLuauFile { name: name.as_ptr(), text: text.as_ptr() }).collect();
        let (script, definitions) = files.split_last().expect("the script is always there");
        let count = i32::try_from(definitions.len()).context("too many definition files")?;
        let mut text = std::ptr::null_mut();
        // SAFETY: `strings` keeps every pointer alive for the call; the text is ours to free.
        let status = unsafe { (self.check)(definitions.as_ptr(), count, script, &mut text) };
        let text = self.take(text);
        match status {
            OK => Ok(text.lines().map(str::to_owned).collect()),
            DEFINITIONS_FAILED => bail!("Luau's type definitions didn't load:\n{}", text.trim_end()),
            other => bail!("the Luau checker failed with status {other}"),
        }
    }

    fn take(&self, text: *mut GdLuauText) -> String {
        if text.is_null() {
            return String::new();
        }
        // SAFETY: `text` came from the wrapper and is freed exactly once, here.
        unsafe {
            let bytes = std::slice::from_raw_parts((self.text_data)(text), (self.text_size)(text));
            let owned = String::from_utf8_lossy(bytes).into_owned();
            (self.text_free)(text);
            owned
        }
    }
}

/// The canvas and widget script APIs, as the type checker reads them.
pub mod definitions {
    pub const COMMON: &str = include_str!("../../glassdeck-luau/types/common.d.luau");
    pub const HOST: &str = include_str!("../../glassdeck-luau/types/host.d.luau");
    pub const OPTIONS_API: &str = include_str!("../../glassdeck-luau/types/options-api.d.luau");
    pub const CANVAS: &str = include_str!("../../glassdeck-luau/types/canvas.d.luau");
    /// A widget's `keyboard.luau`: the `widget` table and the drawing API
    pub const KEYBOARD: &str = include_str!("../../glassdeck-luau/types/keyboard.d.luau");
    /// `options.luau`'s `current`, typed by the generated `options.d.luau`
    pub const CURRENT: &str = "declare current: Options\n";
}

impl LuauChecker {
    /// `options.luau` against the builders, with `current` typed by its own generated `options.d.luau`.
    pub fn check_options(&self, source: &str, options: &str) -> Result<Vec<String>> {
        let definitions = [
            LuauFile { name: "options-api.d.luau", text: definitions::OPTIONS_API },
            LuauFile { name: "options.d.luau", text: options },
            LuauFile { name: "current.d.luau", text: definitions::CURRENT },
        ];
        self.check(&definitions, LuauFile { name: "options.luau", text: source })
    }

    /// `host.luau` against the host API, with `on_options` typed by the generated `options.d.luau` and its
    /// messages by the generated `host-messages.d.luau`.
    pub fn check_host(&self, source: &str, options: &str, messages: &str) -> Result<Vec<String>> {
        let definitions = [
            LuauFile { name: "host-messages.d.luau", text: messages },
            LuauFile { name: "options.d.luau", text: options },
            LuauFile { name: "common.d.luau", text: definitions::COMMON },
            LuauFile { name: "host.d.luau", text: definitions::HOST },
        ];
        self.check(&definitions, LuauFile { name: "host.luau", text: source })
    }
}
