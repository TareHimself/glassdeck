//! An image background: one JPEG at the panel's size, uploaded once and drawn once. Nothing changes
//! after that, so there's no frame loop.

use anyhow::{Context, Result, anyhow, bail};
use glassdeck_package::{Content, Kind, Package};
use glow::HasContext;

use crate::program::compile;

/// Fragment row 0 is the first row of the buffer, which the keyboard scans out at the top, and the
/// texture's row 0 is the picture's top row; so the picture is sampled as it is, unflipped.
const FRAGMENT: &str = r"#version 300 es
precision highp float;
uniform sampler2D picture;
uniform vec2 size;
out vec4 color;
void main() {
    color = vec4(texture(picture, gl_FragCoord.xy / size).rgb, 1.0);
}
";

/// The picture an image background shows, as RGB rows, top row first.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Picture {
    /// The image background's picture, decoded.
    pub fn read(background: &Package) -> Result<Self> {
        let Kind::Background(Content::Image { file }) = &background.manifest().kind else { bail!("{} isn't an image background", background.id()) };
        let entry = background.entry(file).with_context(|| format!("{} holds no {file}", background.id()))?;
        let bytes = background.read_entry(entry).with_context(|| format!("reading {file}"))?;
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).with_context(|| format!("decoding {file}"))?.to_rgb8();
        Ok(Self { width: decoded.width(), height: decoded.height(), rgb: decoded.into_raw() })
    }
}

pub struct ImageProgram {
    program: glow::Program,
    texture: glow::Texture,
}

impl ImageProgram {
    pub fn new(gl: &glow::Context, picture: &Picture) -> Result<Self> {
        let program = compile(gl, FRAGMENT)?;
        // SAFETY: the context is current; the pixels are `width` x `height` RGB, rows packed.
        let texture = unsafe {
            let texture = gl.create_texture().map_err(|e| anyhow!("creating a texture: {e}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGB8 as i32,
                picture.width as i32,
                picture.height as i32,
                0,
                glow::RGB,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&picture.rgb)),
            );
            for (parameter, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, value as i32);
            }
            texture
        };
        Ok(Self { program, texture })
    }

    /// The picture, stretched over `size` (it's made at the panel's size, so normally one texel a pixel).
    pub fn draw(&self, gl: &glow::Context, framebuffer: glow::Framebuffer, size: (u32, u32)) {
        // SAFETY: the context is current and the program and texture are this context's.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.viewport(0, 0, size.0 as i32, size.1 as i32);
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.uniform_1_i32(gl.get_uniform_location(self.program, "picture").as_ref(), 0);
            gl.uniform_2_f32(gl.get_uniform_location(self.program, "size").as_ref(), size.0 as f32, size.1 as f32);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.finish();
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::offscreen::Offscreen;

    #[test]
    fn the_picture_is_drawn_the_right_way_up() {
        let (width, height) = (64u32, 36u32);
        let rgb: Vec<u8> = (0..height).flat_map(|y| (0..width).flat_map(move |_| if y < height / 2 { [250, 20, 20] } else { [20, 20, 250] })).collect();
        let picture = Picture { width, height, rgb };
        let offscreen = Offscreen::new((width, height)).unwrap();
        ImageProgram::new(&offscreen.gl, &picture).unwrap().draw(&offscreen.gl, offscreen.framebuffer(), (width, height));
        let pixels = offscreen.read_rgb();
        // read_rgb puts the last fragment row first; the keyboard scans fragment row 0 out at the top.
        let fragment_row = |row: usize, x: usize| pixels[(height as usize - 1 - row) * width as usize + x];
        let red = |p: u32| (p >> 16) as u8 > 200 && (p as u8) < 60;
        let blue = |p: u32| (p as u8) > 200 && ((p >> 16) as u8) < 60;
        assert!(red(fragment_row(2, 32)), "the picture's top row is fragment row 0: {:#08x}", fragment_row(2, 32));
        assert!(blue(fragment_row(height as usize - 3, 32)), "its bottom row is the last: {:#08x}", fragment_row(height as usize - 3, 32));
    }
}
