//! Slang's compilation API, through `glassdeck_slang`, the small C++ wrapper in
//! `native/glassdeck-slang-native` that silo builds.

pub mod essl;
mod native;
pub mod pack;
pub mod canvas;

use std::ffi::CString;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub use native::Status;

/// A module as source text. Modules load in order, so each may import the ones before it.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub text: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Spirv,
    Glsl,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Declaration {
    Function { name: String, returns: String, params: Vec<Field>, attributes: Vec<Attribute> },
    Struct { name: String, fields: Vec<Field>, attributes: Vec<Attribute> },
    Variable { name: String, r#type: String, attributes: Vec<Attribute> },
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Field {
    pub name: String,
    pub r#type: String,
    #[serde(default)]
    pub default: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<serde_json::Value>,
}

/// Slang failed on the shader itself; `diagnostics` is its output.
#[derive(Debug)]
pub struct ShaderError {
    pub status: Status,
    pub diagnostics: String,
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.status, self.diagnostics.trim())
    }
}

impl std::error::Error for ShaderError {}

pub struct Slang {
    native: native::Library,
}

impl Slang {
    /// Loads `glassdeck_slang` from `dir`, the `bin` folder silo stores it in, with Slang beside it.
    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self { native: native::Library::load(dir)? })
    }

    pub fn has_profile(&self, name: &str) -> bool {
        let name = CString::new(name).expect("profile names have no NUL");
        self.native.has_profile(&name)
    }

    /// The last module's structs, functions and globals, with their user attributes.
    pub fn reflect(&self, sources: &[Source]) -> Result<Vec<Declaration>> {
        let sources = native::Sources::new(sources)?;
        let json = self.native.reflect(&sources)?;
        serde_json::from_str(&json).with_context(|| format!("reading the wrapper's reflection: {json}"))
    }

    /// Compiles the entry point `entry` of the last module.
    pub fn compile(&self, sources: &[Source], entry: &str, target: Target, profile: &str) -> Result<Vec<u8>> {
        if !self.has_profile(profile) {
            bail!("Slang has no profile {profile:?}");
        }
        let sources = native::Sources::new(sources)?;
        let entry = CString::new(entry).context("the entry point name has a NUL")?;
        let profile = CString::new(profile).expect("checked above");
        self.native.compile(&sources, &entry, target, &profile)
    }
}
