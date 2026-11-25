use std::ffi::{CStr, CString, c_char};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::{ShaderError, Source, Target};

/// Mirrors `GdStatus` in `native/glassdeck-slang-native/src/glassdeck_slang.h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    InvalidArgument,
    CompileFailed,
    EntryPointNotFound,
    LinkFailed,
    SlangError,
    OutOfMemory,
    PlatformError,
}

impl Status {
    fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => return None,
            1 => Self::InvalidArgument,
            2 => Self::CompileFailed,
            3 => Self::EntryPointNotFound,
            4 => Self::LinkFailed,
            5 => Self::SlangError,
            6 => Self::OutOfMemory,
            _ => Self::PlatformError,
        })
    }

    fn describes_the_shader(self) -> bool {
        matches!(self, Self::CompileFailed | Self::EntryPointNotFound | Self::LinkFailed)
    }
}

#[repr(C)]
struct GdSource {
    name: *const c_char,
    path: *const c_char,
    text: *const c_char,
}

#[repr(C)]
struct GdText {
    _private: [u8; 0],
}

type Reflect = unsafe extern "C" fn(*const GdSource, i32, *mut *mut GdText, *mut *mut GdText) -> i32;
type Compile = unsafe extern "C" fn(*const GdSource, i32, *const c_char, i32, *const c_char, *mut *mut GdText, *mut *mut GdText) -> i32;
type HasProfile = unsafe extern "C" fn(*const c_char) -> i32;
type TextData = unsafe extern "C" fn(*const GdText) -> *const u8;
type TextSize = unsafe extern "C" fn(*const GdText) -> usize;
type TextFree = unsafe extern "C" fn(*mut GdText);

pub struct Sources {
    _strings: Vec<CString>,
    raw: Vec<GdSource>,
}

impl Sources {
    pub fn new(sources: &[Source]) -> Result<Self> {
        if sources.is_empty() {
            bail!("no Slang modules to load");
        }
        let mut strings = Vec::with_capacity(sources.len() * 3);
        for source in sources {
            for value in [source.name, source.path, source.text] {
                strings.push(CString::new(value).with_context(|| format!("module {} has a NUL in it", source.name))?);
            }
        }
        let raw = strings.chunks(3).map(|s| GdSource { name: s[0].as_ptr(), path: s[1].as_ptr(), text: s[2].as_ptr() }).collect();
        Ok(Self { _strings: strings, raw })
    }

    fn count(&self) -> i32 {
        i32::try_from(self.raw.len()).expect("fewer than 2^31 modules")
    }
}

pub struct Library {
    reflect: Reflect,
    compile: Compile,
    has_profile: HasProfile,
    text_data: TextData,
    text_size: TextSize,
    text_free: TextFree,
    _library: libloading::Library,
}

impl Library {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(libloading::library_filename("glassdeck_slang"));
        let library = open(&path).with_context(|| format!("loading {}", path.display()))?;
        // SAFETY: the symbols are the wrapper's exports, declared with these signatures in its header.
        unsafe {
            Ok(Self {
                reflect: *library.get(b"gd_slang_reflect\0")?,
                compile: *library.get(b"gd_slang_compile\0")?,
                has_profile: *library.get(b"gd_slang_has_profile\0")?,
                text_data: *library.get(b"gd_text_data\0")?,
                text_size: *library.get(b"gd_text_size\0")?,
                text_free: *library.get(b"gd_text_free\0")?,
                _library: library,
            })
        }
    }

    pub fn has_profile(&self, name: &CStr) -> bool {
        // SAFETY: `name` is a valid C string for the call.
        unsafe { (self.has_profile)(name.as_ptr()) != 0 }
    }

    pub fn reflect(&self, sources: &Sources) -> Result<String> {
        let (mut json, mut diagnostics) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: `sources` keeps every string alive for the call; the outputs are ours to free.
        let code = unsafe { (self.reflect)(sources.raw.as_ptr(), sources.count(), &mut json, &mut diagnostics) };
        let json = self.take(json);
        let diagnostics = self.take(diagnostics);
        finish(code, diagnostics)?;
        String::from_utf8(json).context("the reflection isn't UTF-8")
    }

    pub fn compile(&self, sources: &Sources, entry: &CStr, target: Target, profile: &CStr) -> Result<Vec<u8>> {
        let target = match target {
            Target::Spirv => 0,
            Target::Glsl => 1,
        };
        let (mut out, mut diagnostics) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: as in `reflect`.
        let code = unsafe {
            (self.compile)(sources.raw.as_ptr(), sources.count(), entry.as_ptr(), target, profile.as_ptr(), &mut out, &mut diagnostics)
        };
        let out = self.take(out);
        let diagnostics = self.take(diagnostics);
        finish(code, diagnostics)?;
        Ok(out)
    }

    fn take(&self, text: *mut GdText) -> Vec<u8> {
        if text.is_null() {
            return Vec::new();
        }
        // SAFETY: `text` came from the wrapper and is freed exactly once, here.
        unsafe {
            let bytes = std::slice::from_raw_parts((self.text_data)(text), (self.text_size)(text)).to_vec();
            (self.text_free)(text);
            bytes
        }
    }
}

fn finish(code: i32, diagnostics: Vec<u8>) -> Result<()> {
    let Some(status) = Status::from_code(code) else {
        return Ok(());
    };
    let diagnostics = String::from_utf8_lossy(&diagnostics).into_owned();
    if status.describes_the_shader() {
        return Err(ShaderError { status, diagnostics }.into());
    }
    bail!("the Slang wrapper failed with {status:?}: {}", diagnostics.trim())
}

#[cfg(windows)]
fn open(path: &Path) -> Result<libloading::Library, libloading::Error> {
    use libloading::os::windows::{LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library};
    // SAFETY: loading the wrapper runs no initialisers beyond Slang's own.
    unsafe { Library::load_with_flags(path, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS).map(Into::into) }
}

#[cfg(not(windows))]
fn open(path: &Path) -> Result<libloading::Library, libloading::Error> {
    // SAFETY: as above.
    unsafe { libloading::Library::new(path) }
}
