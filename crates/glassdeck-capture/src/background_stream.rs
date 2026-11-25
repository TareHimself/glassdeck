//! Media Foundation reads a background's video entry in place: an `IStream` over its byte range, no copy.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use glassdeck_package::{Background, Entry};
use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL, S_FALSE, S_OK, STG_E_ACCESSDENIED, STG_E_INVALIDFUNCTION};
use windows::Win32::Media::MediaFoundation::{IMFAttributes, IMFByteStream, MF_BYTESTREAM_CONTENT_TYPE, MFCreateMFByteStreamOnStream};
use windows::Win32::System::Com::{
    IStream, IStream_Impl, ISequentialStream_Impl, LOCKTYPE, STATFLAG, STATSTG, STGC, STGTY_STREAM, STREAM_SEEK,
    STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::core::{HRESULT, Interface, Ref, implement, w};

pub fn mp4_byte_stream(background: Arc<Background>, entry: Entry) -> Result<IMFByteStream> {
    let stream: IStream = RangeStream { background, entry, position: AtomicU64::new(0) }.into();
    unsafe {
        let bytes = MFCreateMFByteStreamOnStream(&stream).context("wrapping the background's video for Media Foundation")?;
        bytes.cast::<IMFAttributes>()?.SetString(&MF_BYTESTREAM_CONTENT_TYPE, w!("video/mp4"))?;
        Ok(bytes)
    }
}

#[implement(IStream)]
struct RangeStream {
    background: Arc<Background>,
    entry: Entry,
    position: AtomicU64,
}

impl ISequentialStream_Impl for RangeStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        let buf = unsafe { std::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) };
        let start = self.position.load(Ordering::Acquire);
        let mut filled = 0;
        while filled < buf.len() {
            match self.background.read_range(self.entry, start + filled as u64, &mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => {
                    eprintln!("reading the background's video: {e:#}");
                    return E_FAIL;
                }
            }
        }
        self.position.store(start + filled as u64, Ordering::Release);
        if !pcbread.is_null() {
            unsafe { *pcbread = filled as u32 };
        }
        if filled == buf.len() { S_OK } else { S_FALSE }
    }

    fn Write(&self, _pv: *const core::ffi::c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for RangeStream_Impl {
    fn Seek(&self, dlibmove: i64, dworigin: STREAM_SEEK, plibnewposition: *mut u64) -> windows::core::Result<()> {
        let base = match dworigin {
            STREAM_SEEK_SET => 0,
            STREAM_SEEK_CUR => self.position.load(Ordering::Acquire) as i64,
            STREAM_SEEK_END => self.entry.len as i64,
            _ => return Err(STG_E_INVALIDFUNCTION.into()),
        };
        let target = u64::try_from(base + dlibmove).map_err(|_| windows::core::Error::from(STG_E_INVALIDFUNCTION))?;
        self.position.store(target, Ordering::Release);
        if !plibnewposition.is_null() {
            unsafe { *plibnewposition = target };
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> windows::core::Result<()> {
        Err(STG_E_ACCESSDENIED.into())
    }

    fn CopyTo(&self, _pstm: Ref<IStream>, _cb: u64, _pcbread: *mut u64, _pcbwritten: *mut u64) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn Stat(&self, pstatstg: *mut STATSTG, _grfstatflag: &STATFLAG) -> windows::core::Result<()> {
        unsafe {
            *pstatstg = STATSTG { r#type: STGTY_STREAM.0 as u32, cbSize: self.entry.len, ..Default::default() };
        }
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        let position = AtomicU64::new(self.position.load(Ordering::Acquire));
        Ok(RangeStream { background: self.background.clone(), entry: self.entry, position }.into())
    }
}
