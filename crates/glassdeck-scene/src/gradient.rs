//! The built-in scene: soft colour fields drifting over a vertical gradient. Plain GLSL ES until the
//! Slang toolchain lands. Anything that changes once per frame is worked out here on the CPU, so a
//! pixel only does a handful of multiply-adds.

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

const FRAGMENT: &str = r"#version 300 es
precision mediump float;
uniform highp vec2 u_inverse_resolution;
uniform float u_aspect;
// xy: where a field is centred; z: 1 / radius squared.
uniform vec3 u_fields[3];
out vec4 color;

const vec3 NIGHT = vec3(0.06, 0.04, 0.16);
const vec3 VIOLET = vec3(0.24, 0.11, 0.45);
const vec3 ROSE = vec3(0.84, 0.42, 0.47);
const vec3 TEAL = vec3(0.05, 0.36, 0.52);
const vec3 GLOW = vec3(0.36, 0.17, 0.68);

// Smooth bump: 1 at the centre, 0 from the radius out. Close to a gaussian, without exp.
float field(vec2 p, vec3 f) {
    vec2 d = p - f.xy;
    float x = clamp(1.0 - dot(d, d) * f.z, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}

// Interleaved gradient noise: breaks up 8-bit banding in slow gradients.
highp float dither(highp vec2 pixel) {
    return fract(52.9829189 * fract(dot(pixel, vec2(0.06711056, 0.00583715))));
}

void main() {
    highp vec2 uv = gl_FragCoord.xy * u_inverse_resolution;
    vec2 p = vec2(uv.x * u_aspect, uv.y);
    vec3 c = mix(NIGHT, VIOLET, uv.y);
    c = mix(c, ROSE, field(p, u_fields[0]) * 0.75);
    c = mix(c, TEAL, field(p, u_fields[1]) * 0.7);
    c = mix(c, GLOW, field(p, u_fields[2]) * 0.6);
    c += (dither(gl_FragCoord.xy) - 0.5) / 255.0;
    color = vec4(c, 1.0);
}
";

/// Where the three fields sit at `t`, in the same units as the shader's `p`.
fn fields(t: f32) -> [f32; 9] {
    let t = t * 0.08;
    let field = |x: f32, y: f32, radius: f32| [x, y, 1.0 / (radius * radius)];
    let [a, b, c] = [
        field(0.5 + 0.6 * (t * 1.3).sin(), 0.3 + 0.25 * (t * 0.9).cos(), 0.9),
        field(1.3 + 0.5 * (t * 0.7).cos(), 0.7 + 0.2 * (t * 1.1).sin(), 1.0),
        field(0.9 + 0.7 * (t * 0.5 + 2.0).sin(), 0.5 + 0.3 * (t * 0.8 + 1.0).sin(), 0.75),
    ];
    [a[0], a[1], a[2], b[0], b[1], b[2], c[0], c[1], c[2]]
}

pub struct Gradient {
    program: glow::Program,
    inverse_resolution: Option<glow::UniformLocation>,
    aspect: Option<glow::UniformLocation>,
    fields: Option<glow::UniformLocation>,
}

impl Gradient {
    pub fn new(gl: &glow::Context) -> Result<Self> {
        // SAFETY: the context is current; compile and link results are checked.
        unsafe {
            let program = gl.create_program().map_err(|e| anyhow!("creating a program: {e}"))?;
            let mut shaders = Vec::new();
            for (kind, source) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
                let shader = gl.create_shader(kind).map_err(|e| anyhow!("creating a shader: {e}"))?;
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    bail!("compiling the gradient: {}", gl.get_shader_info_log(shader));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                bail!("linking the gradient: {}", gl.get_program_info_log(program));
            }
            for shader in shaders {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }
            let vertex_array = gl.create_vertex_array().map_err(|e| anyhow!("creating a vertex array: {e}"))?;
            gl.bind_vertex_array(Some(vertex_array));
            gl.use_program(Some(program));
            Ok(Self {
                program,
                inverse_resolution: gl.get_uniform_location(program, "u_inverse_resolution"),
                aspect: gl.get_uniform_location(program, "u_aspect"),
                fields: gl.get_uniform_location(program, "u_fields"),
            })
        }
    }

    /// Draws one frame and waits for the GPU, so the time returned is the frame's real cost.
    pub fn draw(&self, gl: &glow::Context, target: &Target, seconds: f32) -> Duration {
        let started = Instant::now();
        let (w, h) = (target.layout.width as f32, target.layout.height as f32);
        // SAFETY: the context is current and every object here is live.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.framebuffer));
            gl.viewport(0, 0, w as i32, h as i32);
            gl.use_program(Some(self.program));
            gl.uniform_2_f32(self.inverse_resolution.as_ref(), 1.0 / w, 1.0 / h);
            gl.uniform_1_f32(self.aspect.as_ref(), w / h);
            gl.uniform_3_f32_slice(self.fields.as_ref(), &fields(seconds));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.finish();
        }
        started.elapsed()
    }
}
