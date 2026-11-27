//! A half turn for a panel mounted upside down, whose video plane can't turn (Flux's keyboard):
//! frames are drawn into this target, then blitted onto the screen buffer with the destination
//! rectangle reversed on both axes.

use anyhow::{Result, bail};
use glow::HasContext;

pub struct Turn {
    framebuffer: glow::Framebuffer,
    size: (u32, u32),
}

impl Turn {
    pub fn new(gl: &glow::Context, size: (u32, u32)) -> Result<Self> {
        let (width, height) = (size.0 as i32, size.1 as i32);
        // SAFETY: the context is current; the texture and framebuffer are created and bound here.
        unsafe {
            let texture = gl.create_texture().map_err(anyhow::Error::msg)?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, width, height);
            let framebuffer = gl.create_framebuffer().map_err(anyhow::Error::msg)?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(texture), 0);
            if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                bail!("the {width}x{height} target for turning frames isn't complete");
            }
            Ok(Self { framebuffer, size })
        }
    }

    /// Where to draw a frame before it's turned.
    pub fn framebuffer(&self) -> glow::Framebuffer {
        self.framebuffer
    }

    /// Copies the last frame onto `onto`, turned 180 degrees, and waits for the GPU.
    pub fn onto(&self, gl: &glow::Context, onto: glow::Framebuffer) {
        let (width, height) = (self.size.0 as i32, self.size.1 as i32);
        // SAFETY: the context is current and both framebuffers are this context's.
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.framebuffer));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(onto));
            gl.blit_framebuffer(0, 0, width, height, width, height, 0, 0, glow::COLOR_BUFFER_BIT, glow::NEAREST);
            gl.finish();
        }
    }
}
