//! A canvas, built in or a background: the GLSL ES shader `glassdeck pack` compiled from Slang, and the script that
//! writes its inputs. Only inputs written since the last draw are uploaded, and a frame where nothing
//! changed and the shader ignores the clock isn't drawn at all.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail, ensure};
use glassdeck_luau::Print;
use glassdeck_luau::messages::Sent;
use glassdeck_package::{Canvas, CanvasFiles};
use glow::HasContext;

use crate::script::{InputValue, CanvasScript};

const VERTEX: &str = r"#version 300 es
void main() {
    vec2 corner = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
";

/// The glue's uniform holding the inputs, as `glassdeck_slang::canvas::INPUTS_UNIFORM` names it.
const INPUTS_UNIFORM: &str = "gd_inputs";
const PANEL: (f32, f32) = (1920.0, 1080.0);

/// The render target's size for a canvas's scale: even sides, which every scaler handles.
pub fn render_size(scale: f32) -> (u32, u32) {
    let side = |panel: f32| (((panel * scale) / 2.0).round() as u32 * 2).max(2);
    (side(PANEL.0), side(PANEL.1))
}

pub struct CanvasProgram {
    pub canvas: Canvas,
    program: glow::Program,
    /// Active uniforms by their Slang names (`frame.time`), without the `_0` Slang adds to each part.
    uniforms: HashMap<String, glow::UniformLocation>,
    /// The shader reads `frame`, so every frame looks different.
    animated: bool,
    script: Option<CanvasScript>,
    /// Every input's latest value, as the script last wrote it
    values: BTreeMap<String, InputValue>,
    last: Option<f32>,
    index: u32,
    drawn: bool,
}

impl CanvasProgram {
    /// `print`: where `keyboard.luau`'s `print` goes, instead of the process's own output.
    pub fn new(gl: &glow::Context, files: CanvasFiles, print: Option<Print>) -> Result<Self> {
        let CanvasFiles { canvas, fragment, script, messages, .. } = files;
        let program = compile(gl, &fragment)?;
        let uniforms = active_uniforms(gl, program);
        let animated = ["frame.time", "frame.delta", "frame.index"].iter().any(|name| uniforms.contains_key(*name));
        let script = match script {
            Some(source) => Some(CanvasScript::load(&source, messages.as_deref(), &canvas.inputs, print)?),
            None => None,
        };
        Ok(Self { canvas, program, uniforms, animated, script, values: BTreeMap::new(), last: None, index: 0, drawn: false })
    }

    /// The options from the implicit host; a canvas without a script has nothing to give them to.
    pub fn deliver_options(&self, payload: &[u8]) -> Result<()> {
        match &self.script {
            Some(script) => script.deliver_options(payload),
            None => Ok(()),
        }
    }

    /// What the script sent its host half since the last call.
    pub fn take_messages(&self) -> Vec<Sent> {
        self.script.as_ref().map_or_else(Vec::new, CanvasScript::take_messages)
    }

    /// A message from the host half; a canvas without a script has no `bridge.luau` to send it one.
    pub fn deliver_message(&self, message: u16, payload: &[u8]) -> Result<()> {
        match &self.script {
            Some(script) => script.deliver_message(message, payload),
            None => bail!("message {message} for a canvas with no keyboard.luau"),
        }
    }

    /// Every input's latest value, as the script last wrote it.
    pub fn inputs(&self) -> &BTreeMap<String, InputValue> {
        &self.values
    }

    /// Draws a frame if anything changed, and waits for the GPU, so the time returned is the frame's real cost.
    pub fn draw(&mut self, gl: &glow::Context, framebuffer: glow::Framebuffer, size: (u32, u32), seconds: f32) -> Result<Option<Duration>> {
        let started = Instant::now();
        let delta = self.last.map_or(0.0, |last| seconds - last);
        self.last = Some(seconds);
        let changes = match &self.script {
            Some(script) => {
                script.frame(seconds, delta, self.index)?;
                if !self.drawn {
                    let unwritten = script.unwritten();
                    ensure!(unwritten.is_empty(), "keyboard.luau never wrote these inputs: {}", unwritten.join(", "));
                }
                script.take_changes()
            }
            None => BTreeMap::new(),
        };
        if self.drawn && changes.is_empty() && !self.animated {
            return Ok(None);
        }
        let (w, h) = (size.0 as f32, size.1 as f32);
        // SAFETY: the context is current and every object here is live.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.viewport(0, 0, w as i32, h as i32);
            gl.use_program(Some(self.program));
            let uniform = |name: &str| self.uniforms.get(name);
            gl.uniform_1_f32(uniform("frame.time"), seconds);
            gl.uniform_1_f32(uniform("frame.delta"), delta);
            gl.uniform_1_u32(uniform("frame.index"), self.index);
            gl.uniform_2_f32(uniform("frame.resolution"), w, h);
            gl.uniform_2_f32(uniform("frame.panel"), PANEL.0, PANEL.1);
            for (name, value) in &changes {
                let location = uniform(&format!("{INPUTS_UNIFORM}.{name}"));
                match value {
                    InputValue::Float(v) => match v.as_slice() {
                        [x] => gl.uniform_1_f32(location, *x),
                        [x, y] => gl.uniform_2_f32(location, *x, *y),
                        [x, y, z] => gl.uniform_3_f32(location, *x, *y, *z),
                        [x, y, z, w] => gl.uniform_4_f32(location, *x, *y, *z, *w),
                        _ => bail!("input {name} has {} components", v.len()),
                    },
                    InputValue::Uint(n) => gl.uniform_1_u32(location, *n),
                    InputValue::Array(values) => gl.uniform_1_f32_slice(location, values),
                }
            }
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.finish();
        }
        self.values.extend(changes);
        self.index = self.index.wrapping_add(1);
        self.drawn = true;
        Ok(Some(started.elapsed()))
    }
}

/// Links the full-screen triangle with `fragment`, and binds the empty vertex array it draws from.
pub(crate) fn compile(gl: &glow::Context, fragment: &str) -> Result<glow::Program> {
    // SAFETY: the context is current; compile and link results are checked.
    unsafe {
        let program = gl.create_program().map_err(|e| anyhow!("creating a program: {e}"))?;
        let mut shaders = Vec::new();
        for (kind, source) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, fragment)] {
            let shader = gl.create_shader(kind).map_err(|e| anyhow!("creating a shader: {e}"))?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                bail!("compiling a shader: {}", gl.get_shader_info_log(shader));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            bail!("linking a shader: {}", gl.get_program_info_log(program));
        }
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        let vertex_array = gl.create_vertex_array().map_err(|e| anyhow!("creating a vertex array: {e}"))?;
        gl.bind_vertex_array(Some(vertex_array));
        Ok(program)
    }
}

/// The member Slang's std140 wrapper struct keeps an array in.
const STD140_ARRAY_MEMBER: &str = ".data_0";

fn active_uniforms(gl: &glow::Context, program: glow::Program) -> HashMap<String, glow::UniformLocation> {
    let mut uniforms = HashMap::new();
    // SAFETY: the context is current and the program linked.
    unsafe {
        for index in 0..gl.get_active_uniforms(program) {
            let Some(active) = gl.get_active_uniform(program, index) else { continue };
            if let Some(location) = gl.get_uniform_location(program, &active.name) {
                uniforms.insert(slang_name(&active.name), location);
            }
        }
    }
    uniforms
}

/// `gd_inputs_0.roseAt_0` to `gd_inputs.roseAt`, and an array, `gd_inputs_0.bands_0[0]` or as Slang
/// wraps it for std140, `gd_inputs_0.bands_0.data_0[0]`, to `gd_inputs.bands`.
fn slang_name(glsl: &str) -> String {
    let unindexed = glsl.strip_suffix("[0]");
    unindexed
        .map(|array| array.strip_suffix(STD140_ARRAY_MEMBER).unwrap_or(array))
        .unwrap_or(glsl)
        .split('.')
        .map(|part| match part.rsplit_once('_') {
            Some((name, n)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => name,
            _ => part,
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_names_map_back_to_the_inputs_they_hold() {
        assert_eq!(slang_name("gd_inputs_0.roseAt_0"), "gd_inputs.roseAt");
        assert_eq!(slang_name("gd_inputs_0.bands_0[0]"), "gd_inputs.bands");
        assert_eq!(slang_name("gd_inputs_0.levels_0.data_0[0]"), "gd_inputs.levels");
        assert_eq!(slang_name("gd_inputs_0.data_0"), "gd_inputs.data");
    }
}
