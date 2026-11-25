//! Spike: a scene compiled from Slang, given as the GLSL ES fragment shader the packer writes. The
//! gradient's inputs are filled here until keyboard.luau runs in this process.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use glow::HasContext;

use crate::gpu::Target;

const VERTEX: &str = r"#version 300 es
void main() {
    vec2 corner = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
";

pub struct Program {
    program: glow::Program,
    /// Active uniforms by their Slang names (`frame.time`), without the `_0` Slang adds to each part.
    uniforms: HashMap<String, glow::UniformLocation>,
    last: Option<f32>,
    index: u32,
}

impl Program {
    pub fn new(gl: &glow::Context, fragment: &str) -> Result<Self> {
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
            let mut uniforms = HashMap::new();
            for index in 0..gl.get_active_uniforms(program) {
                let Some(active) = gl.get_active_uniform(program, index) else {
                    continue;
                };
                if let Some(location) = gl.get_uniform_location(program, &active.name) {
                    uniforms.insert(slang_name(&active.name), location);
                }
            }
            Ok(Self { program, uniforms, last: None, index: 0 })
        }
    }

    pub fn draw(&mut self, gl: &glow::Context, target: &Target, seconds: f32) -> Duration {
        let started = Instant::now();
        let (w, h) = (target.layout.width as f32, target.layout.height as f32);
        let delta = self.last.map_or(0.0, |last| seconds - last);
        self.last = Some(seconds);
        let t = seconds * 0.08;
        // SAFETY: the context is current and every object here is live.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.framebuffer));
            gl.viewport(0, 0, w as i32, h as i32);
            gl.use_program(Some(self.program));
            if let Some(index) = self.uniforms.get("frame.index") {
                gl.uniform_1_u32(Some(index), self.index);
            }
            for (name, values) in [
                ("frame.time", &[seconds][..]),
                ("frame.delta", &[delta]),
                ("frame.resolution", &[w, h]),
                ("frame.panel", &[1920.0, 1080.0]),
                ("gd_inputs.night", &[0.06, 0.04, 0.16]),
                ("gd_inputs.violet", &[0.24, 0.11, 0.45]),
                ("gd_inputs.rose", &[0.84, 0.42, 0.47]),
                ("gd_inputs.teal", &[0.05, 0.36, 0.52]),
                ("gd_inputs.roseAt", &[0.5 + 0.6 * (t * 1.3).sin(), 0.3 + 0.25 * (t * 0.9).cos()]),
                ("gd_inputs.tealAt", &[1.3 + 0.5 * (t * 0.7).cos(), 0.7 + 0.2 * (t * 1.1).sin()]),
                ("gd_inputs.glowAt", &[0.9 + 0.7 * (t * 0.5 + 2.0).sin(), 0.5 + 0.3 * (t * 0.8 + 1.0).sin()]),
            ] {
                let Some(location) = self.uniforms.get(name) else {
                    continue;
                };
                match *values {
                    [x] => gl.uniform_1_f32(Some(location), x),
                    [x, y] => gl.uniform_2_f32(Some(location), x, y),
                    [x, y, z] => gl.uniform_3_f32(Some(location), x, y, z),
                    _ => unreachable!("the list above has 1 to 3 values per uniform"),
                }
            }
            self.index = self.index.wrapping_add(1);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.finish();
        }
        started.elapsed()
    }
}

/// `gd_inputs_0.roseAt_0` to `gd_inputs.roseAt`.
fn slang_name(glsl: &str) -> String {
    glsl.split('.')
        .map(|part| match part.rsplit_once('_') {
            Some((name, n)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => name,
            _ => part,
        })
        .collect::<Vec<_>>()
        .join(".")
}
