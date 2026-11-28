//! Turns decoded frames 180 degrees for Flux's upside-down panel, whose video plane can't turn,
//! with the RK3566's 2D engine (RGA) through Rockchip's librga (1.10.1 on the keyboard), loaded at
//! run time. The turned copies live in DMA buffers of our own, reused once the display lets go.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::File;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use rustix::ioctl::{Updater, ioctl, opcode};

const LIBRGA: &str = "librga.so.2";
/// `RK_FORMAT_YCbCr_420_SP`: NV12, as MPP decodes.
const NV12: c_int = 0xa << 8;
/// `IM_HAL_TRANSFORM_ROT_180`
const ROTATE_180: c_int = 1 << 1;
/// `IM_STATUS_SUCCESS` and `IM_STATUS_NOERROR`
const DONE: [c_int; 2] = [1, 2];
/// Only the RGA and the display touch these, so the CPU's caches never need flushing.
const HEAP: &str = "/dev/dma_heap/system-uncached";
const RTLD_NOW: c_int = 2;

/// librga's `rga_buffer_t`: 96 bytes, 8-aligned. Only passed between librga's own functions,
/// never read here, so its fields don't matter; on aarch64 a struct this size goes through memory.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct RgaBuffer([u8; 96]);

type WrapFd = unsafe extern "C" fn(c_int, c_int, c_int, c_int, c_int, c_int) -> RgaBuffer;
type Rotate = unsafe extern "C" fn(RgaBuffer, RgaBuffer, c_int, c_int) -> c_int;
type StrError = unsafe extern "C" fn(c_int) -> *const c_char;

unsafe extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

/// `struct dma_heap_allocation_data`
#[repr(C)]
struct HeapAllocation {
    len: u64,
    fd: u32,
    fd_flags: u32,
    heap_flags: u64,
}

const DMA_HEAP_IOCTL_ALLOC: rustix::ioctl::Opcode = opcode::read_write::<HeapAllocation>(b'H', 0);

/// Where a frame's pixels sit in its buffer; the turned copy keeps the same layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: (u32, u32),
    pub strides: (u32, u32),
}

impl Layout {
    fn bytes(self) -> u64 {
        u64::from(self.strides.0) * u64::from(self.strides.1) * 3 / 2
    }
}

pub struct Turner {
    wrap_fd: WrapFd,
    rotate: Rotate,
    str_error: StrError,
    heap: File,
    pool: Arc<Mutex<Pool>>,
}

#[derive(Default)]
struct Pool {
    layout: Option<Layout>,
    free: Vec<OwnedFd>,
}

/// A turned frame's buffer; it goes back to the pool when the display is done with it.
pub struct Turned {
    fd: Option<OwnedFd>,
    layout: Layout,
    pool: Arc<Mutex<Pool>>,
}

impl Turned {
    pub fn dma_buf(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.fd.as_ref().expect("held until dropped").as_fd()
    }
}

impl Drop for Turned {
    fn drop(&mut self) {
        let mut pool = self.pool.lock().expect("RGA pool poisoned");
        if let Some(fd) = self.fd.take()
            && pool.layout == Some(self.layout)
        {
            pool.free.push(fd);
        }
    }
}

impl Turner {
    pub fn open() -> Result<Self> {
        let library = CString::new(LIBRGA).expect("no NUL in the name");
        // SAFETY: dlopen with a valid C string; the library stays loaded for the process's life.
        let handle = unsafe { dlopen(library.as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            bail!("loading {LIBRGA}: {}", last_dl_error());
        }
        let symbol = |name: &str| -> Result<*mut c_void> {
            let name = CString::new(name).expect("no NUL in the name");
            // SAFETY: the handle is live and the name a valid C string.
            let found = unsafe { dlsym(handle, name.as_ptr()) };
            if found.is_null() {
                bail!("{LIBRGA} has no {}: {}", name.to_string_lossy(), last_dl_error());
            }
            Ok(found)
        };
        // SAFETY: librga 1.10 declares these with exactly these C signatures (im2d_buffer.h,
        // im2d_single.h, im2d_common.h).
        let (wrap_fd, rotate, str_error) = unsafe {
            (
                std::mem::transmute::<*mut c_void, WrapFd>(symbol("wrapbuffer_fd_t")?),
                std::mem::transmute::<*mut c_void, Rotate>(symbol("imrotate_t")?),
                std::mem::transmute::<*mut c_void, StrError>(symbol("imStrError_t")?),
            )
        };
        let heap = File::options().read(true).write(true).open(HEAP).with_context(|| format!("opening {HEAP}"))?;
        Ok(Self { wrap_fd, rotate, str_error, heap, pool: Arc::default() })
    }

    /// Copies the frame in `source` into a buffer of our own, turned 180 degrees.
    pub fn turn(&self, source: BorrowedFd<'_>, layout: Layout) -> Result<Turned> {
        let target = self.buffer(layout)?;
        let (width, height) = (layout.size.0 as c_int, layout.size.1 as c_int);
        let (wstride, hstride) = (layout.strides.0 as c_int, layout.strides.1 as c_int);
        // SAFETY: both fds are live DMA buffers holding `layout`'s NV12 frame; librga copies them in.
        let status = unsafe {
            let from = (self.wrap_fd)(source.as_raw_fd(), width, height, wstride, hstride, NV12);
            let into = (self.wrap_fd)(target.as_raw_fd(), width, height, wstride, hstride, NV12);
            (self.rotate)(from, into, ROTATE_180, 1)
        };
        if !DONE.contains(&status) {
            // SAFETY: imStrError_t returns a static string for any status.
            let why = unsafe { CStr::from_ptr((self.str_error)(status)) }.to_string_lossy().into_owned();
            bail!("turning a frame with the RGA: {why} ({status})");
        }
        Ok(Turned { fd: Some(target), layout, pool: Arc::clone(&self.pool) })
    }

    /// A free buffer for `layout`, or a new one; a new layout (another video) empties the pool.
    fn buffer(&self, layout: Layout) -> Result<OwnedFd> {
        {
            let mut pool = self.pool.lock().expect("RGA pool poisoned");
            if pool.layout != Some(layout) {
                *pool = Pool { layout: Some(layout), free: Vec::new() };
            }
            if let Some(fd) = pool.free.pop() {
                return Ok(fd);
            }
        }
        let fd_flags = (rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC).bits();
        let mut allocation = HeapAllocation { len: layout.bytes(), fd: 0, fd_flags, heap_flags: 0 };
        // SAFETY: the heap is open and the struct is the kernel's dma_heap_allocation_data.
        unsafe { ioctl(&self.heap, Updater::<DMA_HEAP_IOCTL_ALLOC, HeapAllocation>::new(&mut allocation)) }
            .with_context(|| format!("allocating a {} byte frame buffer from {HEAP}", layout.bytes()))?;
        // SAFETY: the kernel just gave us this fd.
        Ok(unsafe { OwnedFd::from_raw_fd(allocation.fd as i32) })
    }
}

fn last_dl_error() -> String {
    // SAFETY: dlerror returns null or a thread-local C string valid until the next dl call.
    let error = unsafe { dlerror() };
    if error.is_null() { "no detail".into() } else { unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turned_frame_takes_the_decoders_nv12_layout() {
        let layout = Layout { size: (1920, 1080), strides: (1920, 1088) };
        assert_eq!(layout.bytes(), 1920 * 1088 * 3 / 2);
        assert_eq!(std::mem::size_of::<RgaBuffer>(), 96);
        assert_eq!(std::mem::align_of::<RgaBuffer>(), 8);
    }
}
