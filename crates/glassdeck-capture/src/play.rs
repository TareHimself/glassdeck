use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use glassdeck_package::{Background, Entry};
use glassdeck_proto::Codec;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSourceReader, MF_E_ATTRIBUTENOTFOUND, MF_MT_FRAME_SIZE, MF_MT_MINIMUM_DISPLAY_APERTURE,
    MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_SUBTYPE, MFVideoArea,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_VERSION, MFCreateSourceReaderFromByteStream, MFCreateSourceReaderFromURL, MFSTARTUP_FULL, MFSampleExtension_CleanPoint,
    MFStartup, MFVideoFormat_H264, MFVideoFormat_HEVC,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::HSTRING;

use crate::background_stream::mp4_byte_stream;
use crate::pipeline::{EncodedFrame, EncodedSource, VideoFormat};

const HUNDRED_NS_PER_US: i64 = 10;
const PANEL: (u32, u32) = (1920, 1080);
const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

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

/// Where an MP4 comes from: a file of its own, or an entry read in place from a background.
pub enum Media {
    File(PathBuf),
    BackgroundEntry { background: Arc<Background>, entry: Entry },
}

impl fmt::Display for Media {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(f, "{}", path.display()),
            Self::BackgroundEntry { background, .. } => write!(f, "{}", background.path().display()),
        }
    }
}

/// A video sent as it is, never re-encoded: a paced, encoded source.
pub struct FileSource {
    media: Media,
    looping: bool,
    file: VideoFile,
    format: VideoFormat,
    seq: u32,
    loop_offset_us: u64,
    last_pts_us: u64,
}

impl FileSource {
    pub fn open(media: Media, looping: bool) -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        }
        let file = VideoFile::open(&media)?;
        if (file.width, file.height) != PANEL {
            bail!(
                "{}x{} needs re-encoding to {}x{}, which play does not do yet",
                file.width, file.height, PANEL.0, PANEL.1
            );
        }
        let format = VideoFormat { codec: file.codec, width: file.width as u16, height: file.height as u16 };
        println!("Playing {media} ({:?} {}x{}) without re-encoding", file.codec, file.width, file.height);
        Ok(Self { media, looping, file, format, seq: 0, loop_offset_us: 0, last_pts_us: 0 })
    }
}

impl EncodedSource for FileSource {
    fn format(&self) -> VideoFormat {
        self.format
    }

    fn next_frame(&mut self) -> Result<Option<EncodedFrame>> {
        let sample = match self.file.next_sample()? {
            Some(sample) => sample,
            None if self.looping => {
                self.file = VideoFile::open(&self.media)?;
                self.loop_offset_us = self.last_pts_us + 1;
                match self.file.next_sample()? {
                    Some(sample) => sample,
                    None => bail!("{} has no frames", self.media),
                }
            }
            None => return Ok(None),
        };
        let time_us = sample.pts_us + self.loop_offset_us;
        self.last_pts_us = self.last_pts_us.max(time_us);
        let seq = self.seq;
        self.seq += 1;
        Ok(Some(EncodedFrame { seq, time_us, keyframe: sample.keyframe, data: sample.data, encode_time: Duration::ZERO }))
    }
}

impl VideoFile {
    fn open(media: &Media) -> Result<Self> {
        unsafe {
            let reader = match media {
                Media::File(path) => {
                    let url = HSTRING::from(std::path::absolute(path).context("finding the video file")?.as_os_str());
                    MFCreateSourceReaderFromURL(&url, None).context("opening the video file")?
                }
                Media::BackgroundEntry { background, entry } => {
                    let bytes = mp4_byte_stream(background.clone(), *entry)?;
                    MFCreateSourceReaderFromByteStream(&bytes, None).context("opening the background's video")?
                }
            };
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
            let (width, height) = visible_size(&native)?;
            Ok(Self {
                reader,
                codec,
                width,
                height,
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

/// Encoders pad to whole macroblocks (1080 rows code as 1088); the display aperture holds the picture's real size.
unsafe fn visible_size(media_type: &IMFMediaType) -> Result<(u32, u32)> {
    unsafe {
        if let Some(area) = blob(media_type, &MF_MT_MINIMUM_DISPLAY_APERTURE)? {
            ensure!(area.len() == size_of::<MFVideoArea>(), "display aperture has the wrong size");
            let area: MFVideoArea = std::ptr::read_unaligned(area.as_ptr().cast());
            return Ok((u32::try_from(area.Area.cx)?, u32::try_from(area.Area.cy)?));
        }
        let size = media_type.GetUINT64(&MF_MT_FRAME_SIZE)?;
        Ok(((size >> 32) as u32, size as u32))
    }
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
