//! A canvas, built in or a background: the GLSL ES shaders `glassdeck pack` compiled from Slang, and the script that
//! writes its inputs. Each pass draws into its target, then the canvas draws the screen, reading the
//! targets as textures. Only inputs written since the last draw are uploaded, and a frame where nothing
//! changed and nothing reads the clock or keeps a history isn't drawn at all.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail, ensure};
use glassdeck_luau::Print;
use glassdeck_luau::messages::Sent;
use glassdeck_package::{Canvas, CanvasFiles, RenderTarget, TargetFormat};
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

/// One linked shader, a pass's or the canvas's.
struct Stage {
    program: glow::Program,
    /// Active uniforms by their Slang names (`frame.time`), without the `_0` Slang adds to each part.
    uniforms: HashMap<String, glow::UniformLocation>,
}

impl Stage {
    fn new(gl: &glow::Context, fragment: &str) -> Result<Self> {
        let program = compile(gl, fragment)?;
        Ok(Self { program, uniforms: active_uniforms(gl, program) })
    }

    fn reads_the_clock(&self) -> bool {
        ["frame.time", "frame.delta", "frame.index"].iter().any(|name| self.uniforms.contains_key(*name))
    }
}

/// A target's texture, or with feedback the two it swaps between; `shown` is the one drawn last.
struct Surface {
    target: RenderTarget,
    size: (u32, u32),
    textures: Vec<glow::Texture>,
    framebuffers: Vec<glow::Framebuffer>,
    shown: usize,
}

impl Surface {
    fn new(gl: &glow::Context, target: RenderTarget) -> Result<Self> {
        let size = render_size(target.scale);
        let (internal, kind) = match target.format {
            TargetFormat::Rgba8 => (glow::RGBA8, glow::UNSIGNED_BYTE),
            TargetFormat::Rgba16f => (glow::RGBA16F, glow::HALF_FLOAT),
        };
        let (mut textures, mut framebuffers) = (Vec::new(), Vec::new());
        // SAFETY: the context is current; each object is checked as it's made.
        unsafe {
            for _ in 0..if target.feedback { 2 } else { 1 } {
                let texture = gl.create_texture().map_err(|e| anyhow!("creating target {}: {e}", target.name))?;
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.tex_image_2d(glow::TEXTURE_2D, 0, internal as i32, size.0 as i32, size.1 as i32, 0, glow::RGBA, kind, glow::PixelUnpackData::Slice(None));
                for (parameter, value) in [
                    (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                    (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, value as i32);
                }
                let framebuffer = gl.create_framebuffer().map_err(|e| anyhow!("creating target {}: {e}", target.name))?;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(texture), 0);
                ensure!(
                    gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE,
                    "this GPU can't draw into target {} ({:?})",
                    target.name,
                    target.format
                );
                gl.viewport(0, 0, size.0 as i32, size.1 as i32);
                gl.clear_color(0.0, 0.0, 0.0, 0.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
                textures.push(texture);
                framebuffers.push(framebuffer);
            }
        }
        Ok(Self { target, size, textures, framebuffers, shown: 0 })
    }

    /// Where the pass draws this frame: with feedback, the texture not shown, so it can read the one that is.
    fn drawing(&self) -> usize {
        (self.shown + 1) % self.textures.len()
    }
}

pub struct CanvasProgram {
    pub canvas: Canvas,
    /// Each pass's stage, with the surface it draws into
    passes: Vec<(Stage, usize)>,
    screen: Stage,
    surfaces: Vec<Surface>,
    /// Some shader reads `frame`, or a target keeps a history, so every frame looks different.
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
        let CanvasFiles { canvas, fragment, passes, script, messages, .. } = files;
        ensure!(passes.len() == canvas.passes.len(), "the canvas lists {} passes but has {} shaders", canvas.passes.len(), passes.len());
        let surfaces = canvas.targets.iter().map(|target| Surface::new(gl, target.clone())).collect::<Result<Vec<_>>>()?;
        let passes = canvas
            .passes
            .iter()
            .zip(&passes)
            .map(|(pass, fragment)| {
                let surface = surfaces.iter().position(|s| s.target.name == pass.writes).ok_or_else(|| anyhow!("a pass writes {}, which isn't a target", pass.writes))?;
                Ok((Stage::new(gl, fragment)?, surface))
            })
            .collect::<Result<Vec<_>>>()?;
        let screen = Stage::new(gl, &fragment)?;
        let animated = screen.reads_the_clock() || passes.iter().any(|(stage, _)| stage.reads_the_clock()) || surfaces.iter().any(|s| s.target.feedback);
        let script = match script {
            Some(source) => Some(CanvasScript::load(&source, messages.as_deref(), &canvas.inputs, print)?),
            None => None,
        };
        Ok(Self { canvas, passes, screen, surfaces, animated, script, values: BTreeMap::new(), last: None, index: 0, drawn: false })
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
        let clock = Clock { seconds, delta, index: self.index };
        // SAFETY: the context is current and every object here is live.
        unsafe {
            for (pass, surface) in &self.passes {
                let drawing = self.surfaces[*surface].drawing();
                let size = self.surfaces[*surface].size;
                self.draw_stage(gl, pass, self.surfaces[*surface].framebuffers[drawing], size, clock, &changes)?;
                self.surfaces[*surface].shown = drawing;
            }
            self.draw_stage(gl, &self.screen, framebuffer, size, clock, &changes)?;
            gl.finish();
        }
        self.values.extend(changes);
        self.index = self.index.wrapping_add(1);
        self.drawn = true;
        Ok(Some(started.elapsed()))
    }
}

/// What every stage of one frame is told about time.
#[derive(Clone, Copy)]
struct Clock {
    seconds: f32,
    delta: f32,
    index: u32,
}

impl CanvasProgram {
    /// One full-screen draw of `stage` into `framebuffer`, with the frame's uniforms, the inputs that
    /// changed, and every target bound to the texture unit of its place in the list.
    ///
    /// # Safety
    /// The context is current, and `framebuffer` is live and `size`.
    unsafe fn draw_stage(
        &self,
        gl: &glow::Context,
        stage: &Stage,
        framebuffer: glow::Framebuffer,
        size: (u32, u32),
        clock: Clock,
        changes: &BTreeMap<String, InputValue>,
    ) -> Result<()> {
        let (w, h) = (size.0 as f32, size.1 as f32);
        // SAFETY: as the caller promised.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.viewport(0, 0, w as i32, h as i32);
            gl.use_program(Some(stage.program));
            let uniform = |name: &str| stage.uniforms.get(name);
            gl.uniform_1_f32(uniform("frame.time"), clock.seconds);
            gl.uniform_1_f32(uniform("frame.delta"), clock.delta);
            gl.uniform_1_u32(uniform("frame.index"), clock.index);
            gl.uniform_2_f32(uniform("frame.resolution"), w, h);
            gl.uniform_2_f32(uniform("frame.panel"), PANEL.0, PANEL.1);
            for (unit, surface) in self.surfaces.iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + unit as u32);
                gl.bind_texture(glow::TEXTURE_2D, Some(surface.textures[surface.shown]));
                gl.uniform_1_i32(uniform(&surface.target.name), unit as i32);
            }
            for (name, value) in changes {
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
        }
        Ok(())
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
