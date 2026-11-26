//! A scene background's plan: what `glassdeck pack` worked out from the Slang, for the keyboard's
//! renderer to run it without Slang.

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::check_name;

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
    fn bad_plans_are_refused() {
        let mut twice = plan();
        twice.inputs.push(twice.inputs[0].clone());
        assert!(twice.validate().is_err());
        assert!(ScenePlan { api: 2, ..plan() }.validate().is_err());
        assert!(ScenePlan { scale: 1.5, ..plan() }.validate().is_err());
        assert!(ScenePlan { fragment: "../x".into(), ..plan() }.validate().is_err());
    }
}
