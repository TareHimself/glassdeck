//! Turns the GLSL Slang writes (Vulkan-style `#version 450`, 16-bit types) into GLSL ES 3.00 for the
//! keyboard's GLES 3.0 drivers. Only fragment shaders: the vertex stage is our own fixed triangle.

use anyhow::{Result, bail};

const HEADER: &str = "#version 300 es\nprecision highp float;\nprecision highp int;\n";

/// Lines Slang writes that GLES 3.0 has no use for.
const DROPPED_LINES: [&str; 4] = [
    "#extension GL_EXT_shader_16bit_storage : require",
    "#extension GL_EXT_shader_explicit_arithmetic_types : require",
    "layout(column_major) buffer;",
    "layout(column_major) uniform;",
];

pub fn fragment_to_essl(glsl: &str) -> Result<String> {
    let mut lines = glsl.lines();
    match lines.next() {
        Some(version) if version.starts_with("#version 4") => {}
        other => bail!("expected Slang's #version 4xx line, found {other:?}"),
    }
    let mut body = String::with_capacity(glsl.len());
    for line in lines {
        let trimmed = line.trim();
        if DROPPED_LINES.contains(&trimmed) || is_binding(trimmed) {
            continue;
        }
        if trimmed.starts_with("#extension") {
            bail!("GLSL ES 3.00 has no {trimmed}");
        }
        body.push_str(line);
        body.push('\n');
    }
    let body = unblock_globals(&array_constructors(&lower_half(&body)))?;
    for unsupported in ["buffer ", "gl_VertexIndex", "layout(binding"] {
        if body.contains(unsupported) {
            bail!("the shader uses {unsupported:?}, which GLSL ES 3.00 can't express");
        }
    }
    Ok(format!("{HEADER}{body}"))
}

const UNIFORM_BLOCK: &str = "layout(std140) uniform ";
const GLOBALS_BLOCK: &str = "block_GlobalParams";

/// Slang puts the shader's globals in one uniform block. Panfrost loads a block's values from
/// memory for every pixel but keeps plain uniforms in registers, so each member becomes a plain
/// `uniform`. Any other block is refused: the glue only declares globals.
fn unblock_globals(source: &str) -> Result<String> {
    let Some(start) = source.find(UNIFORM_BLOCK) else {
        return Ok(source.to_owned());
    };
    let name_end = start + source[start..].find('\n').unwrap_or(source.len() - start);
    let name = source[start + UNIFORM_BLOCK.len()..name_end].trim();
    if !name.starts_with(GLOBALS_BLOCK) {
        bail!("the shader declares uniform block {name}; only globals are supported");
    }
    let open = name_end + source[name_end..].find('{').ok_or_else(|| anyhow::anyhow!("block {name} has no body"))?;
    let close = open + source[open..].find('}').ok_or_else(|| anyhow::anyhow!("block {name} isn't closed"))?;
    let end = close + source[close..].find(';').ok_or_else(|| anyhow::anyhow!("block {name} has no instance"))?;
    let instance = source[close + 1..end].trim();
    let members: String = source[open + 1..close]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|member| format!("uniform {member}\n"))
        .collect();
    let rest = format!("{}{members}{}", &source[..start], &source[end + 1..]).replace(&format!("{instance}."), "");
    if rest.contains(UNIFORM_BLOCK) {
        bail!("the shader declares more than one uniform block");
    }
    Ok(rest)
}

/// Slang copies an array (an array input, say) with a C-style `= { a, b };`, which GLSL ES refuses;
/// it becomes the array constructor `= float[2](a, b);`.
fn array_constructors(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            let Some((declared, rest)) = line.split_once(" = {") else { return line.to_owned() };
            let Some(items) = rest.trim_end().strip_suffix("};") else { return line.to_owned() };
            let mut words = declared.split_whitespace().rev();
            let (Some(name), Some(element)) = (words.next(), words.next()) else { return line.to_owned() };
            let Some(length) = name.split_once('[').and_then(|(_, n)| n.strip_suffix(']')) else { return line.to_owned() };
            if items.contains('{') || !length.chars().all(|c| c.is_ascii_digit()) {
                return line.to_owned();
            }
            format!("{declared} = {element}[{length}]({});", items.trim().trim_end_matches(',').trim_end())
        })
        .map(|line| line + "
")
        .collect()
}

fn is_binding(line: &str) -> bool {
    line.starts_with("layout(binding = ") && line.ends_with(')') && line[17..line.len() - 1].chars().all(|c| c.is_ascii_digit())
}

/// `half` becomes `mediump float`: a qualifier where a type is declared, a plain constructor where
/// a value is built (GLSL forbids `mediump float(x)`), and `HF` literals lose their suffix.
fn lower_half(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len() + source.len() / 8);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &source[start..i];
            let constructing = source[i..].trim_start().starts_with('(');
            match half_type(word) {
                Some(full) if constructing => out.push_str(full),
                Some(full) => {
                    out.push_str("mediump ");
                    out.push_str(full);
                }
                None => out.push_str(word),
            }
        } else if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                i += 1;
            }
            let number = &source[start..i];
            out.push_str(number.strip_suffix("HF").or_else(|| number.strip_suffix("hf")).unwrap_or(number));
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

fn half_type(word: &str) -> Option<&'static str> {
    Some(match word {
        "float16_t" => "float",
        "f16vec2" => "vec2",
        "f16vec3" => "vec3",
        "f16vec4" => "vec4",
        "f16mat2" => "mat2",
        "f16mat3" => "mat3",
        "f16mat4" => "mat4",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_declarations_get_a_qualifier_and_constructors_lose_it() {
        let glsl = "#version 450\nfloat16_t bump(vec2 p) { float16_t x = float16_t(p.x); return f16vec3(x).x * 3.0HF; }\n";
        let essl = fragment_to_essl(glsl).unwrap();
        assert!(essl.starts_with("#version 300 es\n"));
        assert!(essl.contains("mediump float bump(vec2 p) { mediump float x = float(p.x); return vec3(x).x * 3.0; }"));
    }

    #[test]
    fn slang_only_lines_are_dropped() {
        let glsl = "#version 450\n#extension GL_EXT_shader_16bit_storage : require\nlayout(column_major) buffer;\nlayout(binding = 1)\nvoid main() {}\n";
        let essl = fragment_to_essl(glsl).unwrap();
        assert!(!essl.contains("binding") && !essl.contains("#extension") && !essl.contains("buffer"));
        assert!(essl.ends_with("void main() {}\n"));
    }

    #[test]
    fn unknown_extensions_and_storage_buffers_are_refused() {
        assert!(fragment_to_essl("#version 450\n#extension GL_EXT_nonuniform_qualifier : require\n").is_err());
        assert!(fragment_to_essl("#version 450\nlayout(std430) buffer B { float x[]; };\n").is_err());
        assert!(fragment_to_essl("void main() {}\n").is_err());
    }

    #[test]
    fn the_globals_block_becomes_plain_uniforms() {
        let glsl = "#version 450\nlayout(binding = 0)\nlayout(std140) uniform block_GlobalParams_std140_0\n{\n    Frame_0 frame_0;\n    Inputs_0 gd_inputs_0;\n}globalParams_0;\nvoid main() { float t = globalParams_0.frame_0.time_0; }\n";
        let essl = fragment_to_essl(glsl).unwrap();
        assert!(essl.contains("uniform Frame_0 frame_0;\nuniform Inputs_0 gd_inputs_0;\n"));
        assert!(essl.contains("float t = frame_0.time_0;"));
        assert!(!essl.contains("globalParams_0"));
    }

    #[test]
    fn other_uniform_blocks_are_refused() {
        assert!(fragment_to_essl("#version 450\nlayout(std140) uniform block_Lights_0\n{\n    vec4 a;\n}lights_0;\n").is_err());
    }

    #[test]
    fn c_style_array_copies_become_constructors() {
        let slang = "    float  _S1[3] = { u.data_0[0], u.data_0[1], u.data_0[2] };
    mediump float  h[2] = { 1.0, 2.0 };
    S s = { 1 };
";
        assert_eq!(
            array_constructors(slang),
            "    float  _S1[3] = float[3](u.data_0[0], u.data_0[1], u.data_0[2]);
    mediump float  h[2] = float[2](1.0, 2.0);
    S s = { 1 };
"
        );
    }

    #[test]
    fn identifiers_containing_half_names_are_left_alone() {
        let essl = fragment_to_essl("#version 450\nfloat my_float16_thing = 1.0;\n").unwrap();
        assert!(essl.contains("float my_float16_thing = 1.0;"));
    }
}
