use std::ffi::c_void;
use std::os::fd::BorrowedFd;
use std::ptr::{NonNull, null_mut};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use glassdeck_proto::{Codec, Mode, StreamInfo};

/// A live access unit yields exactly one picture; this bounds how long we wait for the hardware.
const LIVE_PICTURE_WAIT_MS: i64 = 50;
const NO_WAIT_MS: i64 = 0;
const INPUT_FULL_RETRY: Duration = Duration::from_millis(1);

const PUT_DONE: i32 = 0;
const PUT_FULL: i32 = 1;
const FRAME_NONE: i32 = 0;
const FRAME_READY: i32 = 1;
const FRAME_DROPPED: i32 = 2;

#[repr(C)]
struct RawDecoder {
    _opaque: [u8; 0],
}

#[repr(C)]
struct RawFrame {
    frame: *mut c_void,
    fd: i32,
    width: u32,
    height: u32,
    hor_stride: u32,
    ver_stride: u32,
    pts: i64,
    format: u32,
    is_nv12: u32,
    color_range: u32,
    colorspace: u32,
}

unsafe extern "C" {
    fn gd_mpp_open(hevc: i32, immediate_out: i32, out: *mut *mut RawDecoder) -> i32;
    fn gd_mpp_put(dec: *mut RawDecoder, data: *const u8, len: usize, pts: i64) -> i32;
    fn gd_mpp_get(dec: *mut RawDecoder, timeout_ms: i64, out: *mut RawFrame) -> i32;
    fn gd_mpp_frame_release(frame: *mut c_void);
    fn gd_mpp_close(dec: *mut RawDecoder);
}

/// Rockchip's hardware decoder; pictures stay in decoder-owned DMA buffers so the display can scan them out directly.
pub struct Decoder {
    raw: NonNull<RawDecoder>,
    first_wait_ms: i64,
    dropped: u64,
    pub name: String,
}

// An MPP context may move between threads as long as one thread uses it at a time.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(info: &StreamInfo) -> Result<Self> {
        let live = info.mode == Mode::Live;
        let mut raw = null_mut();
        let ret = unsafe { gd_mpp_open((info.codec == Codec::Hevc) as i32, live as i32, &mut raw) };
        ensure!(ret == 0, "MPP refused to open a {:?} decoder (error {ret})", info.codec);
        Ok(Self {
            raw: NonNull::new(raw).context("MPP returned no decoder")?,
            first_wait_ms: if live { LIVE_PICTURE_WAIT_MS } else { NO_WAIT_MS },
            dropped: 0,
            name: format!("Rockchip MPP {:?}", info.codec),
        })
    }

    /// Feeds one access unit and returns every picture the decoder has ready, in display order.
    pub fn decode(&mut self, access_unit: &[u8], time_us: u64) -> Result<Vec<Picture>> {
        let mut pictures = Vec::new();
        loop {
            let ret = unsafe { gd_mpp_put(self.raw.as_ptr(), access_unit.as_ptr(), access_unit.len(), time_us as i64) };
            match ret {
                PUT_DONE => break,
                PUT_FULL => {
                    while let Some(picture) = self.next_picture(NO_WAIT_MS)? {
                        pictures.push(picture);
                    }
                    thread::sleep(INPUT_FULL_RETRY);
                }
                error => bail!("MPP refused the access unit (error {error})"),
            }
        }
        let mut wait_ms = self.first_wait_ms;
        while let Some(picture) = self.next_picture(wait_ms)? {
            pictures.push(picture);
            wait_ms = NO_WAIT_MS;
        }
        Ok(pictures)
    }

    fn next_picture(&mut self, wait_ms: i64) -> Result<Option<Picture>> {
        loop {
            let mut raw = RawFrame::empty();
            match unsafe { gd_mpp_get(self.raw.as_ptr(), wait_ms, &mut raw) } {
                FRAME_NONE => return Ok(None),
                FRAME_DROPPED => {
                    self.dropped += 1;
                    eprintln!("warning: MPP dropped a corrupt picture ({} so far)", self.dropped);
                }
                FRAME_READY => {
                    let frame = Frame(raw);
                    ensure!(
                        frame.0.is_nv12 != 0,
                        "decoder produced format {:#x}; only 8-bit uncompressed NV12 is supported so far",
                        frame.0.format
                    );
                    return Ok(Some(Picture { time_us: frame.0.pts.max(0) as u64, frame }));
                }
                error => bail!("MPP decode failed (error {error})"),
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { gd_mpp_close(self.raw.as_ptr()) };
    }
}

impl RawFrame {
    fn empty() -> Self {
        Self {
            frame: null_mut(),
            fd: -1,
            width: 0,
            height: 0,
            hor_stride: 0,
            ver_stride: 0,
            pts: 0,
            format: 0,
            is_nv12: 0,
            color_range: 0,
            colorspace: 0,
        }
    }
}

struct Frame(RawFrame);

// The frame is a reference-counted handle to a DMA buffer; releasing it from another thread is allowed.
unsafe impl Send for Frame {}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { gd_mpp_frame_release(self.0.frame) };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Bt601,
    Bt709,
    Bt2020,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Limited,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub encoding: Encoding,
    pub range: Range,
}

/// A decoded NV12 picture in a DMA buffer: luma rows `strides.0` bytes apart, chroma after `strides.1` luma rows.
pub struct Picture {
    pub time_us: u64,
    frame: Frame,
}

impl Picture {
    pub fn dma_buf(&self) -> BorrowedFd<'_> {
        unsafe { BorrowedFd::borrow_raw(self.frame.0.fd) }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.frame.0.width, self.frame.0.height)
    }

    pub fn strides(&self) -> (u32, u32) {
        (self.frame.0.hor_stride, self.frame.0.ver_stride)
    }

    pub fn color(&self) -> Color {
        color_of(self.frame.0.colorspace, self.frame.0.color_range)
    }
}

const MPP_SPC_BT470BG: u32 = 5;
const MPP_SPC_SMPTE170M: u32 = 6;
const MPP_SPC_BT2020_NCL: u32 = 9;
const MPP_SPC_BT2020_CL: u32 = 10;
const MPP_RANGE_JPEG: u32 = 2;

/// Untagged HD video is BT.709 limited range by convention, which is also what our encoder sends.
fn color_of(colorspace: u32, range: u32) -> Color {
    let encoding = match colorspace {
        MPP_SPC_BT470BG | MPP_SPC_SMPTE170M => Encoding::Bt601,
        MPP_SPC_BT2020_NCL | MPP_SPC_BT2020_CL => Encoding::Bt2020,
        _ => Encoding::Bt709,
    };
    let range = if range == MPP_RANGE_JPEG { Range::Full } else { Range::Limited };
    Color { encoding, range }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MPP_SPC_BT709: u32 = 1;

    #[test]
    fn untagged_video_is_bt709_limited() {
        assert_eq!(color_of(2, 0), Color { encoding: Encoding::Bt709, range: Range::Limited });
    }

    #[test]
    fn sd_matrices_map_to_bt601() {
        assert_eq!(color_of(MPP_SPC_SMPTE170M, 1).encoding, Encoding::Bt601);
        assert_eq!(color_of(MPP_SPC_BT470BG, 1).encoding, Encoding::Bt601);
    }

    #[test]
    fn jpeg_range_is_full() {
        assert_eq!(color_of(MPP_SPC_BT709, MPP_RANGE_JPEG).range, Range::Full);
    }
}
