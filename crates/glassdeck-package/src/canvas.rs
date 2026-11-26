//! A canvas background's details: what `glassdeck pack` worked out from the Slang, for the keyboard's
//! renderer to run it without Slang.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{Content, KEYBOARD_SCRIPT, Kind, Origin, Package, SHADER};

/// The `glassdeck` Slang API version this build runs.
pub const CANVAS_API: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Canvas {
    pub api: u32,
    /// Render size as a fraction of the panel
    pub scale: f32,
    pub frame_rate: u32,
    pub inputs: Vec<InputField>,
}

/// One field of the shader's `[Inputs]`, as the uniform the keyboard sets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputField {
    pub name: String,
    pub kind: InputKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputKind {
    /// `float` to `float4` (or `half`), stored as 32-bit floats
    Float {
        components: u8,
    },
    Uint,
    /// `float x[n]` (or `half`)
    FloatArray {
        len: u32,
    },
}

impl Canvas {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.api == CANVAS_API, "the canvas uses glassdeck API {}, not the {CANVAS_API} this build runs", self.api);
        ensure!(self.scale > 0.0 && self.scale <= 1.0, "scale {} is outside 0 to 1", self.scale);
        ensure!((1..=60).contains(&self.frame_rate), "frame rate {} is outside 1 to 60", self.frame_rate);
        for (i, field) in self.inputs.iter().enumerate() {
            ensure!(!self.inputs[..i].iter().any(|f| f.name == field.name), "input {} is listed twice", field.name);
            if let InputKind::Float { components } = field.kind
                && !(1..=4).contains(&components)
            {
                bail!("input {} has {components} components", field.name);
            }
        }
        Ok(())
    }
}

/// What the keyboard needs to run a canvas.
pub struct CanvasFiles {
    pub origin: Origin,
    pub canvas: Canvas,
    pub fragment: String,
    pub script: Option<String>,
}

impl CanvasFiles {
    pub fn read(package: &Package) -> Result<Self> {
        let manifest = package.manifest();
        let Kind::Background(Content::Canvas(canvas)) = &manifest.kind else { bail!("{} isn't a canvas", manifest.id) };
        let fragment = package.text(SHADER)?.with_context(|| format!("{} has no {SHADER}", manifest.id))?;
        Ok(Self { origin: manifest.origin.clone(), canvas: canvas.clone(), fragment, script: package.text(KEYBOARD_SCRIPT)? })
    }
}

/// A digest of the files a package was packed from, so a packed copy can be checked against its
/// sources without the tools that packed it. Line endings are ignored, since checkouts differ.
pub fn source_digest<'a>(files: impl IntoIterator<Item = &'a str>) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for file in files {
        for byte in file.replace("\r\n", "\n").bytes().chain([0]) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
        }
    }
    format!("{hash:016x}")
}

/// What a packed canvas depends on: its `deck.json`, the glassdeck API module, its shader and its scripts.
pub fn canvas_sources(deck: &str, api: &str, shader: &str, scripts: CanvasScripts) -> String {
    source_digest([deck, api, shader, scripts.keyboard.unwrap_or(""), scripts.options.unwrap_or(""), scripts.host.unwrap_or("")])
}

/// A canvas folder's optional scripts.
#[derive(Clone, Copy, Default)]
pub struct CanvasScripts<'a> {
    pub keyboard: Option<&'a str>,
    pub options: Option<&'a str>,
    pub host: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Canvas {
        Canvas {
            api: CANVAS_API,
            scale: 0.5,
            frame_rate: 30,
            inputs: vec![
                InputField { name: "night".into(), kind: InputKind::Float { components: 3 } },
                InputField { name: "spectrum".into(), kind: InputKind::FloatArray { len: 64 } },
            ],
        }
    }

    #[test]
    fn canvas_details_survive_json() {
        let json = serde_json::to_string(&canvas()).unwrap();
        assert!(json.contains(r#""kind":{"type":"float","components":3}"#), "{json}");
        assert_eq!(serde_json::from_str::<Canvas>(&json).unwrap(), canvas());
        canvas().validate().unwrap();
    }

    #[test]
    fn the_digest_ignores_line_endings_but_not_content_or_order() {
        assert_eq!(source_digest(["a\r\nb", "c"]), source_digest(["a\nb", "c"]));
        assert_ne!(source_digest(["a", "b"]), source_digest(["b", "a"]));
        assert_ne!(source_digest(["ab"]), source_digest(["a", "b"]));
    }

    #[test]
    fn bad_canvases_are_refused() {
        let mut twice = canvas();
        twice.inputs.push(twice.inputs[0].clone());
        assert!(twice.validate().is_err());
        assert!(Canvas { api: 2, ..canvas() }.validate().is_err());
        assert!(Canvas { scale: 1.5, ..canvas() }.validate().is_err());
    }
}
