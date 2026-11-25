use std::net::SocketAddr;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glassdeck_proto::{Codec, Frame, Message, Mode, StreamInfo};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSourceReader, MF_E_ATTRIBUTENOTFOUND, MF_MT_FRAME_SIZE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_SUBTYPE,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_VERSION, MFCreateSourceReaderFromURL, MFSTARTUP_FULL, MFSampleExtension_CleanPoint,
    MFStartup, MFVideoFormat_H264, MFVideoFormat_HEVC,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::HSTRING;

use crate::link::{Link, new_stream_id};

/// Frames go out this far ahead of their presentation time, inside the receiver's playout delay.
const SEND_AHEAD: Duration = Duration::from_millis(100);
const HUNDRED_NS_PER_US: i64 = 10;
const PANEL: (u32, u32) = (1920, 1080);
const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

pub struct PlayOptions {
    pub to: SocketAddr,
    pub looping: bool,
    pub link_bytes_per_sec: Option<u64>,
}

struct VideoFile {
    reader: IMFSourceReader,
    codec: Codec,
    width: u32,
    height: u32,
    sequence_header: Option<Vec<u8>>,
}

struct Sample {
    pts_us: u64,
    keyframe: bool,
    data: Vec<u8>,
}

pub fn run(path: &Path, options: PlayOptions) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    let mut file = VideoFile::open(path)?;
    if (file.width, file.height) != PANEL {
        bail!(
            "{}x{} needs re-encoding to {}x{}, which play does not do yet",
            file.width, file.height, PANEL.0, PANEL.1
        );
    }
    let info = StreamInfo {
        stream_id: new_stream_id(),
        codec: file.codec,
        mode: Mode::Paced,
        width: file.width as u16,
        height: file.height as u16,
    };
    println!("Playing {} ({:?} {}x{}) without re-encoding", path.display(), file.codec, file.width, file.height);

    let mut link = Link::connect(options.to, options.link_bytes_per_sec)?;
    link.send(&Message::StreamStart(info))?;

    let started = Instant::now();
    let mut seq = 0;
    let mut loop_offset_us = 0;
    let mut last_pts_us = 0;
    let mut first_pts_us = None;
    loop {
        let Some(sample) = file.next_sample()? else {
            if !options.looping {
                break;
            }
            file = VideoFile::open(path)?;
            loop_offset_us = last_pts_us + 1;
            continue;
        };
        let pts_us = sample.pts_us + loop_offset_us;
        last_pts_us = last_pts_us.max(pts_us);
        let first = *first_pts_us.get_or_insert(pts_us);
        let send_at = started + Duration::from_micros(pts_us.saturating_sub(first));
        thread::sleep(send_at.saturating_duration_since(Instant::now() + SEND_AHEAD));

        link.send(&Message::Frame(Frame {
            stream_id: info.stream_id,
            seq,
            time_us: pts_us,
            keyframe: sample.keyframe,
            payload: sample.data,
        }))?;
        seq += 1;
    }
    link.send(&Message::StreamEnd { stream_id: info.stream_id })?;
    println!("Sent {seq} frames");
    Ok(())
}

impl VideoFile {
    fn open(path: &Path) -> Result<Self> {
        let url = HSTRING::from(std::path::absolute(path).context("finding the video file")?.as_os_str());
        unsafe {
            let reader = MFCreateSourceReaderFromURL(&url, None).context("opening the video file")?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(VIDEO, true)?;
            let native: IMFMediaType = reader.GetNativeMediaType(VIDEO, 0).context("the file has no video stream")?;
            reader.SetCurrentMediaType(VIDEO, None, &native)?;

            let subtype = native.GetGUID(&MF_MT_SUBTYPE)?;
            let codec = if subtype == MFVideoFormat_H264 {
                Codec::H264
            } else if subtype == MFVideoFormat_HEVC {
                Codec::Hevc
            } else {
                bail!("the video codec needs re-encoding, which play does not do yet; use H.264 or HEVC");
            };
            let size = native.GetUINT64(&MF_MT_FRAME_SIZE)?;
            Ok(Self {
                reader,
                codec,
                width: (size >> 32) as u32,
                height: size as u32,
                sequence_header: blob(&native, &MF_MT_MPEG_SEQUENCE_HEADER)?,
            })
        }
    }

    fn next_sample(&mut self) -> Result<Option<Sample>> {
        unsafe {
            loop {
                let mut flags = 0;
                let mut timestamp = 0;
                let mut sample = None;
                self.reader.ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut timestamp), Some(&mut sample))?;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    return Ok(None);
                }
                let Some(sample) = sample else {
                    continue;
                };
                let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).is_ok_and(|v| v != 0);
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut ptr = std::ptr::null_mut();
                let mut len = 0;
                buffer.Lock(&mut ptr, None, Some(&mut len))?;
                let body = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                buffer.Unlock()?;

                let data = match (&self.sequence_header, keyframe) {
                    (Some(header), true) => with_parameter_sets(header, body),
                    _ => body,
                };
                let pts_us = (timestamp / HUNDRED_NS_PER_US).max(0) as u64;
                return Ok(Some(Sample { pts_us, keyframe, data }));
            }
        }
    }
}

/// Keyframes must carry SPS/PPS so a receiver can start (or restart) decoding there.
fn with_parameter_sets(header: &[u8], body: Vec<u8>) -> Vec<u8> {
    let mut data = Vec::with_capacity(header.len() + body.len());
    data.extend_from_slice(header);
    data.extend_from_slice(&body);
    data
}

unsafe fn blob(media_type: &IMFMediaType, key: &windows::core::GUID) -> Result<Option<Vec<u8>>> {
    unsafe {
        let len = match media_type.GetBlobSize(key) {
            Ok(len) => len,
            Err(e) if e.code() == MF_E_ATTRIBUTENOTFOUND => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut data = vec![0u8; len as usize];
        media_type.GetBlob(key, &mut data, None)?;
        Ok(Some(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_sets_go_before_the_keyframe() {
        assert_eq!(with_parameter_sets(&[0, 0, 0, 1, 0x67], vec![0, 0, 0, 1, 0x65]), vec![
            0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65
        ]);
    }
}
