//! A scene background: the GLSL ES shader `glassdeck pack` compiled from Slang, and the script that
//! writes its inputs. Only inputs written since the last draw are uploaded, and a frame where nothing
//! changed and the shader ignores the clock isn't drawn at all.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use glassdeck_package::{Background, Content, ScenePlan};
use glow::HasContext;

use crate::gpu::Target;
use crate::script::{InputValue, SceneScript};

const VERTEX: &str = r"#version 300 es
void main() {
    vec2 corner = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
";

/// The glue's uniform holding the inputs, as `glassdeck_slang::scene::INPUTS_UNIFORM` names it.
const INPUTS_UNIFORM: &str = "gd_inputs";
const PANEL: (f32, f32) = (1920.0, 1080.0);

pub struct SceneProgram {
    pub plan: ScenePlan,
    program: glow::Program,
    /// Active uniforms by their Slang names (`frame.time`), without the `_0` Slang adds to each part.
    uniforms: HashMap<String, glow::UniformLocation>,
    /// The shader reads `frame`, so every frame looks different.
    animated: bool,
    script: Option<SceneScript>,
    last: Option<f32>,
    index: u32,
    drawn: bool,
}

impl SceneProgram {
    pub fn open(gl: &glow::Context, path: &Path) -> Result<Self> {
        let background = Background::open(path)?;
        let Content::Scene { plan } = &background.manifest().content else { bail!("{} isn't a scene", path.display()) };
        let read = |name: &str| -> Result<String> {
            let entry = background.entry(name).with_context(|| format!("the scene has no {name}"))?;
            String::from_utf8(background.read_entry(entry)?).with_context(|| format!("{name} isn't UTF-8"))
        };
        let plan: ScenePlan = serde_json::from_str(&read(plan)?).context("reading the scene's plan")?;
        plan.validate()?;
        let program = compile(gl, &read(&plan.fragment)?)?;
        let uniforms = active_uniforms(gl, program);
        let animated = ["frame.time", "frame.delta", "frame.index"].iter().any(|name| uniforms.contains_key(*name));
        let script = match &plan.script {
            Some(name) => {
                let script = SceneScript::load(&read(name)?, &plan.inputs)?;
                script.apply_settings()?;
                Some(script)
            }
            None => None,
        };
        Ok(Self { plan, program, uniforms, animated, script, last: None, index: 0, drawn: false })
    }

    /// Draws a frame if anything changed, and waits for the GPU, so the time returned is the frame's real cost.
    pub fn draw(&mut self, gl: &glow::Context, target: &Target, seconds: f32) -> Result<Option<Duration>> {
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
        let (w, h) = (target.layout.width as f32, target.layout.height as f32);
        // SAFETY: the context is current and every object here is live.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.framebuffer));
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
        self.index = self.index.wrapping_add(1);
        self.drawn = true;
        Ok(Some(started.elapsed()))
    }
}

fn compile(gl: &glow::Context, fragment: &str) -> Result<glow::Program> {
    // SAFETY: the context is current; compile and link results are checked.
    unsafe {
        let program = gl.create_program().map_err(|e| anyhow!("creating a program: {e}"))?;
        let mut shaders = Vec::new();
        for (kind, source) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, fragment)] {
            let shader = gl.create_shader(kind).map_err(|e| anyhow!("creating a shader: {e}"))?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                bail!("compiling the scene: {}", gl.get_shader_info_log(shader));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            bail!("linking the scene: {}", gl.get_program_info_log(program));
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

/// `gd_inputs_0.roseAt_0` to `gd_inputs.roseAt`, and `gd_inputs_0.bands_0[0]` to `gd_inputs.bands`.
fn slang_name(glsl: &str) -> String {
    glsl.strip_suffix("[0]")
        .unwrap_or(glsl)
        .split('.')
        .map(|part| match part.rsplit_once('_') {
            Some((name, n)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => name,
            _ => part,
        })
        .collect::<Vec<_>>()
        .join(".")
}
