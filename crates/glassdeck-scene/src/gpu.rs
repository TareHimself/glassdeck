//! The GPU through GBM and EGL, with no window: scanout buffers from GBM, each made a GLES
//! framebuffer through a dma-buf EGLImage. libgbm and libEGL load at run time, so the cross-build
//! needs neither.

use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use anyhow::{Context, Result, anyhow, ensure};
use glassdeck_proto::compositor::BufferLayout;
use glow::HasContext;
use khronos_egl as egl;

const DISPLAY_DRIVER: &str = "rockchip";
const GBM_FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const GBM_BO_USE_SCANOUT: u32 = 1 << 0;
const GBM_BO_USE_RENDERING: u32 = 1 << 2;
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
const EGL_PLATFORM_GBM_KHR: egl::Enum = 0x31D7;
const EGL_LINUX_DMA_BUF_EXT: egl::Enum = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: egl::Attrib = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: egl::Attrib = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: egl::Attrib = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: egl::Attrib = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: egl::Attrib = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: egl::Attrib = 0x3444;

type ImageTargetRenderbuffer = unsafe extern "C" fn(target: u32, image: *const c_void);

/// The few libgbm calls we make.
struct Gbm {
    _lib: libloading::Library,
    create_device: unsafe extern "C" fn(fd: i32) -> *mut c_void,
    bo_create: unsafe extern "C" fn(device: *mut c_void, width: u32, height: u32, format: u32, flags: u32) -> *mut c_void,
    bo_get_fd: unsafe extern "C" fn(bo: *mut c_void) -> i32,
    bo_get_stride: unsafe extern "C" fn(bo: *mut c_void) -> u32,
    bo_get_offset: unsafe extern "C" fn(bo: *mut c_void, plane: i32) -> u32,
    bo_get_modifier: unsafe extern "C" fn(bo: *mut c_void) -> u64,
    bo_get_format: unsafe extern "C" fn(bo: *mut c_void) -> u32,
}

impl Gbm {
    fn load() -> Result<Self> {
        // SAFETY: libgbm's initialisers have no preconditions.
        let lib = unsafe { libloading::Library::new("libgbm.so.1") }.context("loading libgbm")?;
        macro_rules! symbol {
            ($name:literal) => {
                // SAFETY: the field's type is the function's C signature from gbm.h.
                *unsafe { lib.get(concat!($name, "\0").as_bytes()) }.context(concat!("libgbm has no ", $name))?
            };
        }
        Ok(Self {
            create_device: symbol!("gbm_create_device"),
            bo_create: symbol!("gbm_bo_create"),
            bo_get_fd: symbol!("gbm_bo_get_fd"),
            bo_get_stride: symbol!("gbm_bo_get_stride"),
            bo_get_offset: symbol!("gbm_bo_get_offset"),
            bo_get_modifier: symbol!("gbm_bo_get_modifier"),
            bo_get_format: symbol!("gbm_bo_get_format"),
            _lib: lib,
        })
    }
}

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}

/// The display card, as glassdeck-local-web uses: GBM on it allocates buffers the display can scan out.
fn open_display_card() -> Result<Card> {
    for entry in fs::read_dir("/dev/dri").context("listing /dev/dri")? {
        let path = entry?.path();
        if !path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("card")) {
            continue;
        }
        let card = Card(OpenOptions::new().read(true).write(true).open(&path).with_context(|| format!("opening {}", path.display()))?);
        if drm::Device::get_driver(&card).is_ok_and(|d| d.name() == std::ffi::OsStr::new(DISPLAY_DRIVER)) {
            return Ok(card);
        }
    }
    Err(anyhow!("no {DISPLAY_DRIVER} display card in /dev/dri"))
}

pub struct Gpu {
    gbm: Gbm,
    device: *mut c_void,
    _card: Card,
    egl: egl::DynamicInstance<egl::EGL1_5>,
    display: egl::Display,
    pub gl: glow::Context,
    image_target_renderbuffer: ImageTargetRenderbuffer,
}

/// A scanout buffer the GPU draws into.
pub struct Target {
    pub dma_buf: OwnedFd,
    pub layout: BufferLayout,
    pub framebuffer: glow::Framebuffer,
}

impl Gpu {
    pub fn open() -> Result<Self> {
        let card = open_display_card()?;
        let gbm = Gbm::load()?;
        // SAFETY: the card fd outlives the device, both living in this struct.
        let device = unsafe { (gbm.create_device)(card.0.as_raw_fd()) };
        ensure!(!device.is_null(), "GBM can't use the display card");

        // SAFETY: libEGL's loader has no preconditions.
        let egl = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }.map_err(|e| anyhow!("loading libEGL 1.5: {e}"))?;
        // SAFETY: `device` is a live gbm_device.
        let display = unsafe { egl.get_platform_display(EGL_PLATFORM_GBM_KHR, device, &[egl::ATTRIB_NONE]) }.context("opening EGL on GBM")?;
        egl.initialize(display).context("initialising EGL")?;
        egl.bind_api(egl::OPENGL_ES_API)?;
        let config = egl
            .choose_first_config(display, &[egl::RENDERABLE_TYPE, egl::OPENGL_ES3_BIT, egl::NONE])?
            .context("EGL offers no GLES 3 config")?;
        let context = egl
            .create_context(display, config, None, &[egl::CONTEXT_MAJOR_VERSION, 3, egl::CONTEXT_MINOR_VERSION, 0, egl::NONE])
            .context("creating a GLES 3.0 context")?;
        egl.make_current(display, None, None, Some(context)).context("using the context with no surface")?;

        let load = |name: &str| egl.get_proc_address(name).map_or(std::ptr::null(), |f| f as *const c_void);
        // SAFETY: the context is current on this thread for the life of the process.
        let gl = unsafe { glow::Context::from_loader_function(load) };
        let target = load("glEGLImageTargetRenderbufferStorageOES");
        ensure!(!target.is_null(), "GLES lacks glEGLImageTargetRenderbufferStorageOES");
        // SAFETY: the pointer is that GL_OES_EGL_image entry point.
        let image_target_renderbuffer = unsafe { std::mem::transmute::<*const c_void, ImageTargetRenderbuffer>(target) };
        Ok(Self { gbm, device, _card: card, egl, display, gl, image_target_renderbuffer })
    }

    pub fn renderer_name(&self) -> String {
        // SAFETY: the context is current.
        unsafe { self.gl.get_parameter_string(glow::RENDERER) }
    }

    pub fn target(&self, width: u32, height: u32) -> Result<Target> {
        let gbm = &self.gbm;
        // SAFETY: `device` is live; GBM returns null on failure, checked below.
        let bo = unsafe { (gbm.bo_create)(self.device, width, height, GBM_FORMAT_XRGB8888, GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING) };
        ensure!(!bo.is_null(), "allocating a {width}x{height} scanout buffer");
        // SAFETY: `bo` is live; it stays allocated for the life of the process, as the buffer on screen.
        let (fd, layout) = unsafe {
            let layout = BufferLayout {
                width,
                height,
                fourcc: (gbm.bo_get_format)(bo),
                stride: (gbm.bo_get_stride)(bo),
                offset: (gbm.bo_get_offset)(bo, 0),
                modifier: (gbm.bo_get_modifier)(bo),
            };
            ((gbm.bo_get_fd)(bo), layout)
        };
        ensure!(fd >= 0, "exporting the scanout buffer");
        // SAFETY: gbm_bo_get_fd hands us a new descriptor.
        let dma_buf = unsafe { OwnedFd::from_raw_fd(fd) };

        let mut attribs = vec![
            egl::WIDTH as egl::Attrib,
            width as egl::Attrib,
            egl::HEIGHT as egl::Attrib,
            height as egl::Attrib,
            EGL_LINUX_DRM_FOURCC_EXT,
            layout.fourcc as egl::Attrib,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            fd as egl::Attrib,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            layout.offset as egl::Attrib,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            layout.stride as egl::Attrib,
        ];
        if layout.modifier != DRM_FORMAT_MOD_INVALID {
            attribs.extend([
                EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
                (layout.modifier & 0xffff_ffff) as egl::Attrib,
                EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
                (layout.modifier >> 32) as egl::Attrib,
            ]);
        }
        attribs.push(egl::ATTRIB_NONE);
        // SAFETY: dma-buf imports take no context and no client buffer.
        let image = unsafe {
            self.egl.create_image(
                self.display,
                egl::Context::from_ptr(egl::NO_CONTEXT),
                EGL_LINUX_DMA_BUF_EXT,
                egl::ClientBuffer::from_ptr(std::ptr::null_mut()),
                &attribs,
            )
        }
        .context("importing the scanout buffer into EGL")?;

        let gl = &self.gl;
        // SAFETY: the context is current; every object is checked before use.
        unsafe {
            let renderbuffer = gl.create_renderbuffer().map_err(|e| anyhow!("creating a renderbuffer: {e}"))?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(renderbuffer));
            (self.image_target_renderbuffer)(glow::RENDERBUFFER, image.as_ptr());
            let framebuffer = gl.create_framebuffer().map_err(|e| anyhow!("creating a framebuffer: {e}"))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::RENDERBUFFER, Some(renderbuffer));
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            ensure!(status == glow::FRAMEBUFFER_COMPLETE, "the scanout buffer isn't a complete framebuffer ({status:#x})");
            Ok(Target { dma_buf, layout, framebuffer })
        }
    }
}
