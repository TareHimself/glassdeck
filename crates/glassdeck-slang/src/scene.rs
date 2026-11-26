//! Reads a scene's shape from Slang's reflection and writes the glue module around it: the real
//! entry point, the `[Inputs]` uniforms (as 32-bit floats) and the conversion to the user's types.

use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use glassdeck_package::{InputField, InputKind};

use crate::{Attribute, Declaration, Field};

/// What a render scale and frame rate are when the scene doesn't say.
const DEFAULT_SCALE: f32 = 0.5;
const DEFAULT_FRAME_RATE: u32 = 30;
/// The glue's uniform holding the inputs; the keyboard sets `gd_inputs.<field>`.
pub const INPUTS_UNIFORM: &str = "gd_inputs";
pub const FRAGMENT_ENTRY: &str = "gd_fragment";

/// The one `[Scene]` function and the `[Inputs]` it reads.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneShape {
    pub function: String,
    pub scale: f32,
    pub frame_rate: u32,
    pub inputs: Option<Inputs>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inputs {
    pub name: String,
    pub fields: Vec<InputSlot>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputSlot {
    pub field: InputField,
    /// The field's Slang type, which the glue converts to
    pub slang_type: String,
    pub has_initializer: bool,
}

impl SceneShape {
    pub fn read(declarations: &[Declaration]) -> Result<Self> {
        let marked: Vec<_> = declarations
            .iter()
            .filter_map(|d| match d {
                Declaration::Function { name, returns, params, attributes } if has(attributes, "Scene") => {
                    Some((name, returns, params, attributes))
                }
                _ => None,
            })
            .collect();
        for declaration in declarations {
            if let Declaration::Function { name, attributes, .. } = declaration {
                for unsupported in ["Effect", "Pass"] {
                    ensure!(!has(attributes, unsupported), "{name} is marked [{unsupported}], which isn't supported yet");
                }
            }
        }
        let (name, returns, params, attributes) = match marked.as_slice() {
            [one] => *one,
            [] => bail!("no function is marked [Scene]"),
            many => bail!("several functions are marked [Scene]: {}", many.iter().map(|m| m.0.as_str()).collect::<Vec<_>>().join(", ")),
        };
        ensure!(
            matches!(returns.as_str(), "half4" | "float4"),
            "[Scene] function {name} returns {returns}; it must return half4 or float4"
        );
        let inputs = read_inputs(declarations)?;
        match (params.as_slice(), &inputs) {
            ([p], _) if p.r#type == "Pixel" => ensure!(inputs.is_none(), "the shader declares [Inputs] but {name} doesn't take them"),
            ([p, i], Some(inputs)) if p.r#type == "Pixel" && i.r#type == inputs.name => {}
            _ => bail!(
                "[Scene] function {name} must take (Pixel) or (Pixel, {}), not ({})",
                inputs.as_ref().map_or("Inputs", |i| &i.name),
                types(params)
            ),
        }
        let (mut scale, mut frame_rate) = (DEFAULT_SCALE, DEFAULT_FRAME_RATE);
        for attribute in attributes {
            match attribute.name.as_str() {
                "Scene" => {}
                "Scale" => scale = number(attribute, name)? as f32,
                "FrameRate" => frame_rate = number(attribute, name)? as u32,
                other => bail!("[{other}] doesn't go on a [Scene] function ({name})"),
            }
        }
        ensure!(scale > 0.0 && scale <= 1.0, "[Scale({scale})] on {name} must be above 0 and at most 1");
        ensure!((1..=60).contains(&frame_rate), "[FrameRate({frame_rate})] on {name} must be 1 to 60");
        Ok(Self { function: name.clone(), scale, frame_rate, inputs })
    }

    /// Without a script nothing writes the inputs, so every field needs an initializer.
    pub fn check_without_script(&self) -> Result<()> {
        let Some(inputs) = &self.inputs else { return Ok(()) };
        let unset: Vec<&str> = inputs.fields.iter().filter(|s| !s.has_initializer).map(|s| s.field.name.as_str()).collect();
        ensure!(unset.is_empty(), "there's no keyboard.luau to write the inputs, and these have no initializer: {}", unset.join(", "));
        Ok(())
    }

    pub fn input_fields(&self) -> Vec<InputField> {
        self.inputs.iter().flat_map(|i| i.fields.iter().map(|s| s.field.clone())).collect()
    }

    /// The glue module, importing `module`; with no script the inputs come from their initializers.
    pub fn glue(&self, module: &str, scripted: bool) -> String {
        let mut out = format!("import glassdeck;\nimport {module};\n\n");
        let call = match &self.inputs {
            None => format!("{}(p)", self.function),
            Some(inputs) if !scripted => {
                let _ = writeln!(out, "{0} gd_load() {{\n    {0} i = {{}};\n    return i;\n}}\n", inputs.name);
                format!("{}(p, gd_load())", self.function)
            }
            Some(inputs) => {
                out.push_str("struct GdInputs {\n");
                for slot in &inputs.fields {
                    let _ = writeln!(out, "    {};", storage(slot));
                }
                let _ = writeln!(out, "}};\n\nuniform GdInputs {INPUTS_UNIFORM};\n\n{0} gd_load() {{\n    {0} i;", inputs.name);
                for slot in &inputs.fields {
                    let _ = writeln!(out, "    {}", load(slot));
                }
                out.push_str("    return i;\n}\n\n");
                format!("{}(p, gd_load())", self.function)
            }
        };
        let _ = write!(
            out,
            "[shader(\"fragment\")]\nfloat4 {FRAGMENT_ENTRY}(float4 at : SV_Position) : SV_Target {{\n    Pixel p;\n    p.uv = at.xy / frame.resolution;\n    p.position = p.uv * frame.panel;\n    return float4({call});\n}}\n"
        );
        out
    }
}

fn read_inputs(declarations: &[Declaration]) -> Result<Option<Inputs>> {
    let marked: Vec<_> = declarations
        .iter()
        .filter_map(|d| match d {
            Declaration::Struct { name, fields, attributes } if has(attributes, "Inputs") => Some((name, fields)),
            _ => None,
        })
        .collect();
    let (name, fields) = match marked.as_slice() {
        [] => return Ok(None),
        [one] => *one,
        many => bail!("several structs are marked [Inputs]: {}", many.iter().map(|m| m.0.as_str()).collect::<Vec<_>>().join(", ")),
    };
    let fields = fields
        .iter()
        .map(|f| {
            let kind = kind_of(&f.r#type).with_context(|| format!("[Inputs] field {}", f.name))?;
            Ok(InputSlot { field: InputField { name: f.name.clone(), kind }, slang_type: f.r#type.clone(), has_initializer: f.default })
        })
        .collect::<Result<_>>()?;
    Ok(Some(Inputs { name: name.clone(), fields }))
}

fn kind_of(slang_type: &str) -> Result<InputKind> {
    if slang_type == "uint" {
        return Ok(InputKind::Uint);
    }
    if let Some((element, rest)) = slang_type.split_once('[') {
        let len: u32 =
            rest.strip_suffix(']').and_then(|n| n.parse().ok()).with_context(|| format!("can't read the array type {slang_type}"))?;
        ensure!(matches!(element, "half" | "float"), "arrays of {element} aren't supported, only half and float");
        ensure!(len > 0, "a zero-length array can't be an input");
        return Ok(InputKind::FloatArray { len });
    }
    let components = match slang_type.strip_prefix("half").or_else(|| slang_type.strip_prefix("float")) {
        Some("") => 1,
        Some(n @ ("2" | "3" | "4")) => n.parse().expect("matched a digit"),
        _ => bail!("{slang_type} can't be an input; use half, float (2 to 4 wide), uint, or an array of half or float"),
    };
    Ok(InputKind::Float { components })
}

/// The 32-bit field the keyboard sets.
fn storage(slot: &InputSlot) -> String {
    let name = &slot.field.name;
    match slot.field.kind {
        InputKind::Float { components: 1 } => format!("float {name}"),
        InputKind::Float { components } => format!("float{components} {name}"),
        InputKind::Uint => format!("uint {name}"),
        InputKind::FloatArray { len } => format!("float {name}[{len}]"),
    }
}

/// Copies one stored field into the user's type, converting to `half` where they asked for it.
fn load(slot: &InputSlot) -> String {
    let name = &slot.field.name;
    match slot.field.kind {
        InputKind::FloatArray { len } => {
            let element = slot.slang_type.split('[').next().expect("split always yields one part");
            format!("for (int k = 0; k < {len}; k++) i.{name}[k] = {element}({INPUTS_UNIFORM}.{name}[k]);")
        }
        _ => format!("i.{name} = {}({INPUTS_UNIFORM}.{name});", slot.slang_type),
    }
}

fn has(attributes: &[Attribute], name: &str) -> bool {
    attributes.iter().any(|a| a.name == name)
}

fn number(attribute: &Attribute, function: &str) -> Result<f64> {
    match attribute.args.as_slice() {
        [value] => value.as_f64().with_context(|| format!("[{}] on {function} needs a number", attribute.name)),
        _ => bail!("[{}] on {function} takes one number", attribute.name),
    }
}

fn types(params: &[Field]) -> String {
    params.iter().map(|p| p.r#type.as_str()).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn attribute(name: &str, args: Vec<serde_json::Value>) -> Attribute {
        Attribute { name: name.into(), args }
    }

    fn field(name: &str, r#type: &str, default: bool) -> Field {
        Field { name: name.into(), r#type: r#type.into(), default }
    }

    fn gradient(attributes: Vec<Attribute>) -> Vec<Declaration> {
        vec![
            Declaration::Struct {
                name: "Inputs".into(),
                fields: vec![field("night", "half3", false), field("roseAt", "float2", false), field("bands", "half[8]", true)],
                attributes: vec![attribute("Inputs", vec![])],
            },
            Declaration::Function {
                name: "gradient".into(),
                returns: "half4".into(),
                params: vec![field("p", "Pixel", false), field("i", "Inputs", false)],
                attributes,
            },
        ]
    }

    #[test]
    fn the_scene_function_its_options_and_inputs_are_read() {
        let shape = SceneShape::read(&gradient(vec![attribute("Scene", vec![]), attribute("FrameRate", vec![json!(60)])])).unwrap();
        assert_eq!((shape.function.as_str(), shape.scale, shape.frame_rate), ("gradient", 0.5, 60));
        assert_eq!(
            shape.input_fields(),
            [
                InputField { name: "night".into(), kind: InputKind::Float { components: 3 } },
                InputField { name: "roseAt".into(), kind: InputKind::Float { components: 2 } },
                InputField { name: "bands".into(), kind: InputKind::FloatArray { len: 8 } },
            ]
        );
    }

    #[test]
    fn the_glue_stores_floats_and_converts_to_the_users_types() {
        let glue = SceneShape::read(&gradient(vec![attribute("Scene", vec![])])).unwrap().glue("scene", true);
        assert!(glue.contains("    float3 night;\n    float2 roseAt;\n    float bands[8];\n"), "{glue}");
        assert!(glue.contains("uniform GdInputs gd_inputs;"));
        assert!(glue.contains("i.night = half3(gd_inputs.night);"));
        assert!(glue.contains("for (int k = 0; k < 8; k++) i.bands[k] = half(gd_inputs.bands[k]);"));
        assert!(glue.contains("return float4(gradient(p, gd_load()));"));
    }

    #[test]
    fn without_a_script_the_initializers_are_used_and_required() {
        let shape = SceneShape::read(&gradient(vec![attribute("Scene", vec![])])).unwrap();
        let glue = shape.glue("scene", false);
        assert!(glue.contains("Inputs i = {};") && !glue.contains("uniform"), "{glue}");
        let error = shape.check_without_script().unwrap_err().to_string();
        assert!(error.contains("night, roseAt") && !error.contains("bands"), "{error}");
    }

    #[test]
    fn mistakes_are_named() {
        let error = |d: &[Declaration]| SceneShape::read(d).unwrap_err().to_string();
        assert!(error(&gradient(vec![])).contains("no function is marked [Scene]"));
        assert!(
            error(&gradient(vec![attribute("Scene", vec![]), attribute("Reach", vec![json!(2)])]))
                .contains("[Reach] doesn't go on a [Scene]")
        );
        assert!(error(&gradient(vec![attribute("Scene", vec![]), attribute("Scale", vec![json!(2)])])).contains("at most 1"));
        let mut bad = gradient(vec![attribute("Scene", vec![])]);
        if let Declaration::Struct { fields, .. } = &mut bad[0] {
            fields.push(field("m", "float4x4", false));
        }
        assert!(format!("{:#}", SceneShape::read(&bad).unwrap_err()).contains("float4x4 can't be an input"));
    }
}
