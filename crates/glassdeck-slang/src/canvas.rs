//! Reads a canvas's shape from Slang's reflection and writes the glue module around each of its
//! functions: the real entry point, the `[Inputs]` uniforms (as 32-bit floats) and the conversion to
//! the user's types. A canvas is its `[Canvas]` function, plus any `[Pass]` functions drawn first
//! into `[Target]` textures.

use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use glassdeck_package::{InputField, InputKind, MAX_PASSES, MAX_TARGETS, Pass, RenderTarget, TargetFormat};

use crate::{Attribute, Declaration, Field};

/// What a render scale and frame rate are when the canvas doesn't say.
const DEFAULT_SCALE: f32 = 0.5;
const DEFAULT_FRAME_RATE: u32 = 30;
/// The glue's uniform holding the inputs; the keyboard sets `gd_inputs.<field>`.
pub const INPUTS_UNIFORM: &str = "gd_inputs";
pub const FRAGMENT_ENTRY: &str = "gd_fragment";

/// What a `[Target]`'s type must be: a texture with its sampler, so GLSL ES reads it as `sampler2D`.
const TARGET_TYPES: [&str; 2] = ["Sampler2D<vector<half,4>>", "Sampler2D<vector<float,4>>"];

/// The one `[Canvas]` function, the `[Pass]` functions drawn before it, their targets, and the `[Inputs]` they read.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasShape {
    pub function: String,
    pub scale: f32,
    pub frame_rate: u32,
    pub inputs: Option<Inputs>,
    pub targets: Vec<RenderTarget>,
    /// In drawing order
    pub passes: Vec<PassShape>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PassShape {
    pub function: String,
    pub writes: String,
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

impl CanvasShape {
    pub fn read(declarations: &[Declaration]) -> Result<Self> {
        let marked: Vec<_> = declarations
            .iter()
            .filter_map(|d| match d {
                Declaration::Function { name, returns, params, attributes } if has(attributes, "Canvas") => {
                    Some((name, returns, params, attributes))
                }
                _ => None,
            })
            .collect();
        for declaration in declarations {
            if let Declaration::Function { name, attributes, .. } = declaration {
                ensure!(!has(attributes, "Effect"), "{name} is marked [Effect], which isn't supported yet");
            }
        }
        let (name, returns, params, attributes) = match marked.as_slice() {
            [one] => *one,
            [] => bail!("no function is marked [Canvas]"),
            many => bail!("several functions are marked [Canvas]: {}", many.iter().map(|m| m.0.as_str()).collect::<Vec<_>>().join(", ")),
        };
        let inputs = read_inputs(declarations)?;
        check_entry("Canvas", name, returns, params, inputs.as_ref())?;
        let (mut scale, mut frame_rate) = (DEFAULT_SCALE, DEFAULT_FRAME_RATE);
        for attribute in attributes {
            match attribute.name.as_str() {
                "Canvas" => {}
                "Scale" => scale = number(attribute, name)? as f32,
                "FrameRate" => frame_rate = number(attribute, name)? as u32,
                other => bail!("[{other}] doesn't go on a [Canvas] function ({name})"),
            }
        }
        ensure!(scale > 0.0 && scale <= 1.0, "[Scale({scale})] on {name} must be above 0 and at most 1");
        ensure!((1..=60).contains(&frame_rate), "[FrameRate({frame_rate})] on {name} must be 1 to 60");
        let targets = read_targets(declarations)?;
        let passes = read_passes(declarations, inputs.as_ref())?;
        let canvas = glassdeck_package::Canvas {
            api: glassdeck_package::CANVAS_API,
            scale,
            frame_rate,
            inputs: Vec::new(),
            targets: targets.clone(),
            passes: passes.iter().map(|p| Pass { writes: p.writes.clone() }).collect(),
        };
        canvas.validate()?;
        Ok(Self { function: name.clone(), scale, frame_rate, inputs, targets, passes })
    }

    /// Every function to compile, the passes first in drawing order, then the canvas.
    pub fn functions(&self) -> impl Iterator<Item = &str> {
        self.passes.iter().map(|p| p.function.as_str()).chain([self.function.as_str()])
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

    /// `inputs.d.luau`: the script's `inputs`, typed from `[Inputs]`, for editors and the pack-time check.
    pub fn inputs_definitions(&self) -> String {
        let mut out = String::from(
            "-- Generated by glassdeck pack from the shader's [Inputs]; rewritten on every pack, so don't edit it.\ndeclare inputs: {\n",
        );
        for slot in self.inputs.iter().flat_map(|i| &i.fields) {
            let luau = match slot.field.kind {
                InputKind::Float { components: 1 } | InputKind::Uint => "number",
                InputKind::Float { .. } => "vector",
                InputKind::FloatArray { .. } => "{ number }",
            };
            let _ = writeln!(out, "    {}: {luau}, -- {}", slot.field.name, slot.slang_type);
        }
        out.push_str("}\n");
        out
    }

    /// The glue module around `function`, importing `module`; with no script the inputs come from their initializers.
    pub fn glue(&self, function: &str, module: &str, scripted: bool) -> String {
        let mut out = format!("import glassdeck;\nimport {module};\n\n");
        let call = match &self.inputs {
            None => format!("{function}(p)"),
            Some(inputs) if !scripted => {
                let _ = writeln!(out, "{0} gd_load() {{\n    {0} i = {{}};\n    return i;\n}}\n", inputs.name);
                format!("{function}(p, gd_load())")
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
                format!("{function}(p, gd_load())")
            }
        };
        let _ = write!(
            out,
            "[shader(\"fragment\")]\nfloat4 {FRAGMENT_ENTRY}(float4 at : SV_Position) : SV_Target {{\n    Pixel p;\n    p.uv = at.xy / frame.resolution;\n    p.position = p.uv * frame.panel;\n    return float4({call});\n}}\n"
        );
        out
    }
}

/// A `[Canvas]` or `[Pass]` function: `(Pixel)`, or `(Pixel, Inputs)` when the shader has inputs; returns a colour.
fn check_entry(mark: &str, name: &str, returns: &str, params: &[Field], inputs: Option<&Inputs>) -> Result<()> {
    ensure!(matches!(returns, "half4" | "float4"), "[{mark}] function {name} returns {returns}; it must return half4 or float4");
    match (params, inputs) {
        ([p], _) if p.r#type == "Pixel" => ensure!(inputs.is_none(), "the shader declares [Inputs] but {name} doesn't take them"),
        ([p, i], Some(inputs)) if p.r#type == "Pixel" && i.r#type == inputs.name => {}
        _ => bail!(
            "[{mark}] function {name} must take (Pixel) or (Pixel, {}), not ({})",
            inputs.map_or("Inputs", |i| &i.name),
            types(params)
        ),
    }
    Ok(())
}

fn read_targets(declarations: &[Declaration]) -> Result<Vec<RenderTarget>> {
    let mut targets = Vec::new();
    for declaration in declarations {
        let Declaration::Variable { name, r#type, attributes } = declaration else { continue };
        if !has(attributes, "Target") {
            for option in ["Feedback", "Format"] {
                ensure!(!has(attributes, option), "[{option}] goes on a [Target], and {name} isn't one");
            }
            continue;
        }
        ensure!(TARGET_TYPES.contains(&r#type.as_str()), "[Target] {name} must be a Sampler2D<half4> or Sampler2D<float4>, not {}", r#type);
        let mut target = RenderTarget { name: name.clone(), scale: DEFAULT_SCALE, format: TargetFormat::Rgba8, feedback: false };
        for attribute in attributes {
            match attribute.name.as_str() {
                "Target" => {}
                "Feedback" => target.feedback = true,
                "Scale" => target.scale = number(attribute, name)? as f32,
                "Format" => {
                    target.format = match attribute.args.as_slice() {
                        [serde_json::Value::String(f)] if f == "rgba8" => TargetFormat::Rgba8,
                        [serde_json::Value::String(f)] if f == "rgba16f" => TargetFormat::Rgba16f,
                        _ => bail!("[Format] on {name} takes \"rgba8\" or \"rgba16f\""),
                    }
                }
                other => bail!("[{other}] doesn't go on a [Target] ({name})"),
            }
        }
        targets.push(target);
    }
    ensure!(targets.len() <= MAX_TARGETS, "the shader has {} targets; at most {MAX_TARGETS}", targets.len());
    Ok(targets)
}

fn read_passes(declarations: &[Declaration], inputs: Option<&Inputs>) -> Result<Vec<PassShape>> {
    let mut passes = Vec::new();
    for declaration in declarations {
        let Declaration::Function { name, returns, params, attributes } = declaration else { continue };
        if !has(attributes, "Pass") {
            ensure!(!has(attributes, "Writes"), "[Writes] goes on a [Pass], and {name} isn't one");
            continue;
        }
        check_entry("Pass", name, returns, params, inputs)?;
        let (mut order, mut writes) = (None, None);
        for attribute in attributes {
            match attribute.name.as_str() {
                "Pass" => order = Some(number(attribute, name)?),
                "Writes" => {
                    writes = match attribute.args.as_slice() {
                        [serde_json::Value::String(target)] => Some(target.clone()),
                        _ => bail!("[Writes] on {name} takes the name of a [Target]"),
                    }
                }
                other => bail!("[{other}] doesn't go on a [Pass] ({name})"),
            }
        }
        let order = order.expect("marked [Pass]");
        let writes = writes.with_context(|| format!("[Pass] {name} needs [Writes(\"target\")] to say which [Target] it draws"))?;
        ensure!(order >= 0.0 && order.fract() == 0.0, "[Pass({order})] on {name} must be 0, 1, 2 ...");
        passes.push((order as u32, PassShape { function: name.clone(), writes }));
    }
    passes.sort_by_key(|(order, _)| *order);
    if let Some(pair) = passes.windows(2).find(|w| w[0].0 == w[1].0) {
        bail!("{} and {} are both [Pass({})]", pair[0].1.function, pair[1].1.function, pair[0].0);
    }
    ensure!(passes.len() <= MAX_PASSES, "the shader has {} passes; at most {MAX_PASSES}", passes.len());
    Ok(passes.into_iter().map(|(_, pass)| pass).collect())
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
    fn the_canvas_function_its_options_and_inputs_are_read() {
        let shape = CanvasShape::read(&gradient(vec![attribute("Canvas", vec![]), attribute("FrameRate", vec![json!(60)])])).unwrap();
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
        let glue = CanvasShape::read(&gradient(vec![attribute("Canvas", vec![])])).unwrap().glue("gradient", "canvas", true);
        assert!(glue.contains("    float3 night;\n    float2 roseAt;\n    float bands[8];\n"), "{glue}");
        assert!(glue.contains("uniform GdInputs gd_inputs;"));
        assert!(glue.contains("i.night = half3(gd_inputs.night);"));
        assert!(glue.contains("for (int k = 0; k < 8; k++) i.bands[k] = half(gd_inputs.bands[k]);"));
        assert!(glue.contains("return float4(gradient(p, gd_load()));"));
    }

    #[test]
    fn inputs_become_a_typed_luau_declaration() {
        let definitions = CanvasShape::read(&gradient(vec![attribute("Canvas", vec![])])).unwrap().inputs_definitions();
        assert!(
            definitions.contains(
                "declare inputs: {\n    night: vector, -- half3\n    roseAt: vector, -- float2\n    bands: { number }, -- half[8]\n}\n"
            ),
            "{definitions}"
        );
    }

    #[test]
    fn without_a_script_the_initializers_are_used_and_required() {
        let shape = CanvasShape::read(&gradient(vec![attribute("Canvas", vec![])])).unwrap();
        let glue = shape.glue("gradient", "canvas", false);
        assert!(glue.contains("Inputs i = {};") && !glue.contains("uniform"), "{glue}");
        let error = shape.check_without_script().unwrap_err().to_string();
        assert!(error.contains("night, roseAt") && !error.contains("bands"), "{error}");
    }

    fn trail(target: Vec<Attribute>, pass: Vec<Attribute>) -> Vec<Declaration> {
        let mut declarations = gradient(vec![attribute("Canvas", vec![])]);
        declarations.push(Declaration::Variable { name: "trail".into(), r#type: "Sampler2D<vector<half,4>>".into(), attributes: target });
        declarations.push(Declaration::Function {
            name: "smear".into(),
            returns: "half4".into(),
            params: vec![field("p", "Pixel", false), field("i", "Inputs", false)],
            attributes: pass,
        });
        declarations
    }

    #[test]
    fn targets_and_the_passes_that_write_them_are_read() {
        let target = vec![attribute("Target", vec![]), attribute("Feedback", vec![]), attribute("Scale", vec![json!(0.25)]), attribute("Format", vec![json!("rgba16f")])];
        let shape = CanvasShape::read(&trail(target, vec![attribute("Pass", vec![json!(0)]), attribute("Writes", vec![json!("trail")])])).unwrap();
        assert_eq!(shape.targets, [RenderTarget { name: "trail".into(), scale: 0.25, format: TargetFormat::Rgba16f, feedback: true }]);
        assert_eq!(shape.passes, [PassShape { function: "smear".into(), writes: "trail".into() }]);
        assert_eq!(shape.functions().collect::<Vec<_>>(), ["smear", "gradient"]);
        assert!(shape.glue("smear", "canvas", true).contains("return float4(smear(p, gd_load()));"));
    }

    #[test]
    fn target_and_pass_mistakes_are_named() {
        let error = |d: &[Declaration]| format!("{:#}", CanvasShape::read(d).unwrap_err());
        let target = || vec![attribute("Target", vec![])];
        assert!(error(&trail(target(), vec![attribute("Pass", vec![json!(0)])])).contains("needs [Writes(\"target\")]"));
        assert!(error(&trail(target(), vec![attribute("Pass", vec![json!(0)]), attribute("Writes", vec![json!("smoke")])])).contains("smoke, which isn't a target"));
        assert!(error(&trail(target(), vec![])).contains("written by 0 passes"));
        assert!(error(&trail(vec![attribute("Feedback", vec![])], vec![])).contains("[Feedback] goes on a [Target]"));
        let format = vec![attribute("Target", vec![]), attribute("Format", vec![json!("rgb565")])];
        assert!(error(&trail(format, vec![attribute("Pass", vec![json!(0)]), attribute("Writes", vec![json!("trail")])])).contains("takes \"rgba8\" or \"rgba16f\""));
    }

    #[test]
    fn mistakes_are_named() {
        let error = |d: &[Declaration]| CanvasShape::read(d).unwrap_err().to_string();
        assert!(error(&gradient(vec![])).contains("no function is marked [Canvas]"));
        assert!(
            error(&gradient(vec![attribute("Canvas", vec![]), attribute("Reach", vec![json!(2)])]))
                .contains("[Reach] doesn't go on a [Canvas]")
        );
        assert!(error(&gradient(vec![attribute("Canvas", vec![]), attribute("Scale", vec![json!(2)])])).contains("at most 1"));
        let mut bad = gradient(vec![attribute("Canvas", vec![])]);
        if let Declaration::Struct { fields, .. } = &mut bad[0] {
            fields.push(field("m", "float4x4", false));
        }
        assert!(format!("{:#}", CanvasShape::read(&bad).unwrap_err()).contains("float4x4 can't be an input"));
    }
}
