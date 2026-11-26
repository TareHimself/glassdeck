//! A scene background's plan: what `glassdeck pack` worked out from the Slang, for the keyboard's
//! renderer to run it without Slang.

use std::io::Cursor;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::zip::read_directory;
use crate::{Background, Content, MANIFEST, Manifest, Origin, check_name};

pub const SCENE_PLAN: &str = "scene/plan.json";
pub const SCENE_FRAGMENT: &str = "scene/fragment.essl";
pub const SCENE_SCRIPT: &str = "scene/keyboard.luau";
/// The `glassdeck` Slang API version this build runs.
pub const SCENE_API: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScenePlan {
    pub api: u32,
    /// Render size as a fraction of the panel
    pub scale: f32,
    pub frame_rate: u32,
    /// Entry holding the GLSL ES 3.00 fragment shader
    pub fragment: String,
    /// Entry holding `keyboard.luau`, which writes the inputs; without one the shader uses its initializers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
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

/// What the keyboard needs to run a scene, read from a scene `.deckbg` on disk or in memory.
pub struct SceneFiles {
    pub origin: Origin,
    pub plan: ScenePlan,
    pub fragment: String,
    pub script: Option<String>,
}

impl SceneFiles {
    pub fn read(background: &Background) -> Result<Self> {
        Self::parse(background.manifest(), |name| {
            let entry = background.entry(name).with_context(|| format!("the scene has no {name}"))?;
            background.read_entry(entry)
        })
    }

    /// A scene `.deckbg` built into a program, such as the built-in gradient.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let entries = read_directory(&mut Cursor::new(bytes))?;
        let read = |name: &str| -> Result<Vec<u8>> {
            let entry = entries.get(name).with_context(|| format!("the scene has no {name}"))?;
            Ok(bytes[entry.offset as usize..(entry.offset + entry.len) as usize].to_vec())
        };
        let manifest: Manifest = serde_json::from_slice(&read(MANIFEST)?).context("parsing the scene's manifest")?;
        manifest.validate(|name| entries.contains_key(name))?;
        Self::parse(&manifest, read)
    }

    fn parse(manifest: &Manifest, read: impl Fn(&str) -> Result<Vec<u8>>) -> Result<Self> {
        let Content::Scene { plan } = &manifest.content else { bail!("{} isn't a scene", manifest.title) };
        let text = |name: &str| -> Result<String> { String::from_utf8(read(name)?).with_context(|| format!("{name} isn't UTF-8")) };
        let plan: ScenePlan = serde_json::from_str(&text(plan)?).context("reading the scene's plan")?;
        plan.validate()?;
        let fragment = text(&plan.fragment)?;
        let script = plan.script.as_deref().map(text).transpose()?;
        Ok(Self { origin: manifest.origin.clone(), plan, fragment, script })
    }
}

/// A digest of the files a scene was packed from, so a packed copy can be checked against its
/// sources without Slang. Line endings are ignored, since checkouts differ.
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

/// What a packed scene depends on: the glassdeck API module, its shader and its script.
pub fn scene_sources(api: &str, shader: &str, script: Option<&str>) -> String {
    source_digest([api, shader, script.unwrap_or("")])
}

impl ScenePlan {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.api == SCENE_API, "the scene uses glassdeck API {}, not the {SCENE_API} this build runs", self.api);
        ensure!(self.scale > 0.0 && self.scale <= 1.0, "scale {} is outside 0 to 1", self.scale);
        ensure!((1..=60).contains(&self.frame_rate), "frame rate {} is outside 1 to 60", self.frame_rate);
        for name in std::iter::once(&self.fragment).chain(&self.script) {
            check_name(name)?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> ScenePlan {
        ScenePlan {
            api: SCENE_API,
            scale: 0.5,
            frame_rate: 30,
            fragment: SCENE_FRAGMENT.into(),
            script: Some(SCENE_SCRIPT.into()),
            inputs: vec![
                InputField { name: "night".into(), kind: InputKind::Float { components: 3 } },
                InputField { name: "spectrum".into(), kind: InputKind::FloatArray { len: 64 } },
            ],
        }
    }

    #[test]
    fn a_plan_survives_json() {
        let json = serde_json::to_string(&plan()).unwrap();
        assert!(json.contains(r#""kind":{"type":"float","components":3}"#), "{json}");
        assert_eq!(serde_json::from_str::<ScenePlan>(&json).unwrap(), plan());
        plan().validate().unwrap();
    }

    #[test]
    fn the_digest_ignores_line_endings_but_not_content_or_order() {
        assert_eq!(source_digest(["a\r\nb", "c"]), source_digest(["a\nb", "c"]));
        assert_ne!(source_digest(["a", "b"]), source_digest(["b", "a"]));
        assert_ne!(source_digest(["ab"]), source_digest(["a", "b"]));
    }

    #[test]
    fn bad_plans_are_refused() {
        let mut twice = plan();
        twice.inputs.push(twice.inputs[0].clone());
        assert!(twice.validate().is_err());
        assert!(ScenePlan { api: 2, ..plan() }.validate().is_err());
        assert!(ScenePlan { scale: 1.5, ..plan() }.validate().is_err());
        assert!(ScenePlan { fragment: "../x".into(), ..plan() }.validate().is_err());
    }
}
