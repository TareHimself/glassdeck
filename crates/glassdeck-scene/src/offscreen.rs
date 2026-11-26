//! An OpenGL context with no visible window, for drawing scenes on the PC: a hidden window for WGL,
//! a 4.3 core context (the first desktop version that takes GLSL ES 3.00, as the keyboard does),
//! and a framebuffer the scene draws into and the preview reads back.

use std::ffi::{CStr, c_void};

use anyhow::{Context, Result, anyhow, bail};
use glow::HasContext;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Gdi::{GetDC, HDC, ReleaseDC};
use windows::Win32::Graphics::OpenGL::{
    ChoosePixelFormat, HGLRC, PFD_DOUBLEBUFFER, PFD_DRAW_TO_WINDOW, PFD_MAIN_PLANE, PFD_SUPPORT_OPENGL, PFD_TYPE_RGBA,
    PIXELFORMATDESCRIPTOR, SetPixelFormat, wglCreateContext, wglDeleteContext, wglGetProcAddress, wglMakeCurrent,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW};
use windows::core::{PCSTR, w};

const WGL_CONTEXT_MAJOR_VERSION_ARB: i32 = 0x2091;
const WGL_CONTEXT_MINOR_VERSION_ARB: i32 = 0x2092;
const WGL_CONTEXT_PROFILE_MASK_ARB: i32 = 0x9126;
const WGL_CONTEXT_CORE_PROFILE_BIT_ARB: i32 = 0x1;
/// Desktop GL accepts `#version 300 es` from 4.3 (ARB_ES3_compatibility).
const GL_VERSION: (i32, i32) = (4, 3);

type CreateContextAttribs = unsafe extern "system" fn(HDC, HGLRC, *const i32) -> HGLRC;

/// Current on the thread that made it, which must be the only one to use it.
pub struct Offscreen {
    pub gl: glow::Context,
    framebuffer: glow::Framebuffer,
    size: (u32, u32),
    window: HWND,
    dc: HDC,
    context: HGLRC,
}

impl Offscreen {
    pub fn new(size: (u32, u32)) -> Result<Self> {
        // SAFETY: plain Win32 and WGL calls on handles made here; each result is checked.
        unsafe {
            let window = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("glassdeck scene"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .context("creating a hidden window for OpenGL")?;
            let dc = GetDC(Some(window));
            let format = PIXELFORMATDESCRIPTOR {
                nSize: size_of::<PIXELFORMATDESCRIPTOR>() as u16,
                nVersion: 1,
                dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
                iPixelType: PFD_TYPE_RGBA,
                cColorBits: 32,
                iLayerType: PFD_MAIN_PLANE.0 as u8,
                ..Default::default()
            };
            let chosen = ChoosePixelFormat(dc, &format);
            if chosen == 0 {
                bail!("no OpenGL pixel format");
            }
            SetPixelFormat(dc, chosen, &format).context("setting the OpenGL pixel format")?;
            let legacy = wglCreateContext(dc).context("creating an OpenGL context")?;
            wglMakeCurrent(dc, legacy).context("making the OpenGL context current")?;
            let create = wglGetProcAddress(PCSTR(c"wglCreateContextAttribsARB".as_ptr().cast()))
                .context("the OpenGL driver has no wglCreateContextAttribsARB")?;
            let create: CreateContextAttribs = std::mem::transmute(create);
            let attributes = [
                WGL_CONTEXT_MAJOR_VERSION_ARB,
                GL_VERSION.0,
                WGL_CONTEXT_MINOR_VERSION_ARB,
                GL_VERSION.1,
                WGL_CONTEXT_PROFILE_MASK_ARB,
                WGL_CONTEXT_CORE_PROFILE_BIT_ARB,
                0,
            ];
            let context = create(dc, HGLRC::default(), attributes.as_ptr());
            wglMakeCurrent(dc, HGLRC::default()).context("releasing the first OpenGL context")?;
            wglDeleteContext(legacy).context("deleting the first OpenGL context")?;
            if context.is_invalid() {
                bail!("the OpenGL driver can't make a {}.{} core context, which GLSL ES 3.00 needs", GL_VERSION.0, GL_VERSION.1);
            }
            wglMakeCurrent(dc, context).context("making the OpenGL context current")?;
            let opengl32 = LoadLibraryW(w!("opengl32.dll")).context("loading opengl32.dll")?;
            let gl = glow::Context::from_loader_function_cstr(|name| load(opengl32, name));
            let framebuffer = target(&gl, size)?;
            Ok(Self { gl, framebuffer, size, window, dc, context })
        }
    }

    pub fn framebuffer(&self) -> glow::Framebuffer {
        self.framebuffer
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn renderer_name(&self) -> String {
        // SAFETY: the context is current on this thread.
        unsafe { self.gl.get_parameter_string(glow::RENDERER) }
    }

    /// The last frame as 0RGB pixels, top row first.
    pub fn read_rgb(&self) -> Vec<u32> {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let mut rgba = vec![0u8; w * h * 4];
        // SAFETY: the context is current and the framebuffer is live and this size.
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer));
            self.gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut rgba)));
        }
        rgba.chunks_exact(w * 4).rev().flat_map(|row| row.as_chunks::<4>().0.iter().map(|p| u32::from_be_bytes([0, p[0], p[1], p[2]]))).collect()
    }
}

impl Drop for Offscreen {
    fn drop(&mut self) {
        // SAFETY: the handles were made in `new` and are released once, here.
        unsafe {
            let _ = wglMakeCurrent(self.dc, HGLRC::default());
            let _ = wglDeleteContext(self.context);
            ReleaseDC(Some(self.window), self.dc);
            let _ = DestroyWindow(self.window);
        }
    }
}

/// WGL returns 1.1 functions only from opengl32.dll, and some drivers return small integers for missing ones.
fn load(opengl32: HMODULE, name: &CStr) -> *const c_void {
    let name = PCSTR(name.as_ptr().cast());
    // SAFETY: `name` is a valid C string; the results are function pointers or null.
    unsafe {
        match wglGetProcAddress(name) {
            Some(f) if !matches!(f as usize, 1 | 2 | 3 | usize::MAX) => f as *const c_void,
            _ => GetProcAddress(opengl32, name).map_or(std::ptr::null(), |f| f as *const c_void),
        }
    }
}

fn target(gl: &glow::Context, (w, h): (u32, u32)) -> Result<glow::Framebuffer> {
    // SAFETY: the context is current; completeness is checked.
    unsafe {
        let colour = gl.create_renderbuffer().map_err(|e| anyhow!("creating a renderbuffer: {e}"))?;
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(colour));
        gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, w as i32, h as i32);
        let framebuffer = gl.create_framebuffer().map_err(|e| anyhow!("creating a framebuffer: {e}"))?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::RENDERBUFFER, Some(colour));
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            bail!("the preview framebuffer isn't complete ({status:#x})");
        }
        Ok(framebuffer)
    }
}
