//! Media Foundation: probe a video, and re-encode one into the deck spec (HEVC at the panel's size).

use std::mem::ManuallyDrop;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use glassdeck_deck::{FrameRate, PANEL, ProbedCodec, ProbedVideo};
use glassdeck_proto::Codec;
use windows::Win32::Foundation::RECT;
use windows::Win32::Media::MediaFoundation::{
    CLSID_VideoProcessorMFT, CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncMPVGOPSize, ICodecAPI, IMFAttributes,
    IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader, IMFTransform, IMFVideoProcessorControl,
    MF_E_ATTRIBUTENOTFOUND, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFCreateAttributes,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFSTARTUP_FULL, MFStartup, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12,
    MFVideoArea, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};
use windows::core::{GUID, HSTRING, Interface};

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const KEYFRAME_SECONDS: u32 = 2;

#[derive(Clone, Copy, Debug)]
pub struct EncodeSettings {
    pub bitrate: u32,
}

pub fn start_media_foundation() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    Ok(())
}

pub fn probe(path: &Path) -> Result<ProbedVideo> {
    unsafe {
        let reader = open_reader(path, None)?;
        let native = reader.GetNativeMediaType(VIDEO, 0).context("the file has no video stream")?;
        let subtype = native.GetGUID(&MF_MT_SUBTYPE)?;
        let codec = match subtype {
            s if s == MFVideoFormat_H264 => ProbedCodec::Known(Codec::H264),
            s if s == MFVideoFormat_HEVC => ProbedCodec::Known(Codec::Hevc),
            _ => ProbedCodec::Other,
        };
        let (width, height) = visible_size(&native)?;
        let (numerator, denominator) = unpack(native.GetUINT64(&MF_MT_FRAME_RATE).context("the video has no frame rate")?);
        Ok(ProbedVideo {
            codec,
            width: u16::try_from(width)?,
            height: u16::try_from(height)?,
            frame_rate: FrameRate { numerator, denominator },
        })
    }
}

/// The part of a `src`-sized picture that fills `dst` without stretching: centred, the overflow cut off.
pub fn cover(src: (u32, u32), dst: (u32, u32)) -> RECT {
    let (sw, sh) = (src.0 as u64, src.1 as u64);
    let (dw, dh) = (dst.0 as u64, dst.1 as u64);
    let even = |v: u64| (v & !1) as i32;
    if sw * dh > dw * sh {
        let w = sh * dw / dh;
        let left = (sw - w) / 2;
        RECT { left: even(left), top: 0, right: even(left) + even(w), bottom: sh as i32 }
    } else {
        let h = sw * dh / dw;
        let top = (sh - h) / 2;
        RECT { left: 0, top: even(top), right: sw as i32, bottom: even(top) + even(h) }
    }
}

/// Decodes `src`, crops and scales it to fill the panel, and encodes HEVC into the MP4 `dst`.
pub fn transcode(src: &Path, dst: &Path, settings: EncodeSettings) -> Result<FrameRate> {
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.context("MFCreateAttributes returned nothing")?;
        attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        let reader = open_reader(src, Some(&attributes))?;

        let native = reader.GetNativeMediaType(VIDEO, 0).context("the file has no video stream")?;
        let display = visible_size(&native)?;
        let rate = native.GetUINT64(&MF_MT_FRAME_RATE).context("the video has no frame rate")?;
        let decoded_type = nv12_type(display, rate)?;
        reader.SetCurrentMediaType(VIDEO, None, &decoded_type).context("no decoder turns this video into NV12")?;
        let decoded = unpack(reader.GetCurrentMediaType(VIDEO)?.GetUINT64(&MF_MT_FRAME_SIZE)?);

        let panel = (PANEL.0 as u32, PANEL.1 as u32);
        let scaler = Scaler::new(decoded, cover(display, panel), panel, rate)?;
        let (writer, stream) = hevc_writer(dst, panel, rate, settings)?;
        writer.BeginWriting()?;

        let started = Instant::now();
        let mut first_time = None;
        let mut frames = 0u64;
        loop {
            let mut flags = 0;
            let mut timestamp = 0;
            let mut sample = None;
            reader.ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut timestamp), Some(&mut sample))?;
            if let Some(sample) = sample {
                let first = *first_time.get_or_insert(timestamp);
                sample.SetSampleTime(timestamp - first)?;
                for scaled in scaler.push(&sample)? {
                    writer.WriteSample(stream, &scaled)?;
                    frames += 1;
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        for scaled in scaler.drain()? {
            writer.WriteSample(stream, &scaled)?;
            frames += 1;
        }
        writer.Finalize().context("finishing the MP4")?;
        if frames == 0 {
            bail!("{} decoded to no frames", src.display());
        }
        println!("Encoded {frames} frames in {:.1}s", started.elapsed().as_secs_f64());
        let (numerator, denominator) = unpack(rate);
        Ok(FrameRate { numerator, denominator })
    }
}

/// The video processor MFT, cropping to `source` and scaling to `out`, in system memory.
struct Scaler {
    transform: IMFTransform,
    out_bytes: u32,
    provides_samples: bool,
}

impl Scaler {
    unsafe fn new(input: (u32, u32), source: RECT, out: (u32, u32), rate: u64) -> Result<Self> {
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER).context("creating the video processor")?;
            transform.SetInputType(0, &nv12_type(input, rate)?, 0).context("video processor input")?;
            transform.SetOutputType(0, &nv12_type(out, rate)?, 0).context("video processor output")?;
            let control: IMFVideoProcessorControl = transform.cast()?;
            control.SetSourceRectangle(Some(&source))?;
            control.SetDestinationRectangle(Some(&RECT { left: 0, top: 0, right: out.0 as i32, bottom: out.1 as i32 }))?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            let info = transform.GetOutputStreamInfo(0)?;
            Ok(Self {
                transform,
                out_bytes: info.cbSize.max(out.0 * out.1 * 3 / 2),
                provides_samples: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
            })
        }
    }

    unsafe fn push(&self, sample: &IMFSample) -> Result<Vec<IMFSample>> {
        unsafe {
            self.transform.ProcessInput(0, sample, 0).context("scaling a frame")?;
            self.collect()
        }
    }

    unsafe fn drain(&self) -> Result<Vec<IMFSample>> {
        unsafe {
            self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
            self.collect()
        }
    }

    unsafe fn collect(&self) -> Result<Vec<IMFSample>> {
        let mut out = Vec::new();
        unsafe {
            loop {
                let provided = if self.provides_samples { None } else { Some(self.blank()?) };
                let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(provided),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let result = self.transform.ProcessOutput(0, &mut buffers, &mut status);
                let [buffer] = &mut buffers;
                let sample = ManuallyDrop::take(&mut buffer.pSample);
                drop(ManuallyDrop::take(&mut buffer.pEvents));
                match result {
                    Ok(()) => out.push(sample.context("the video processor returned no frame")?),
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(out),
                    Err(e) => return Err(e).context("scaling a frame"),
                }
            }
        }
    }

    unsafe fn blank(&self) -> Result<IMFSample> {
        unsafe {
            let sample = MFCreateSample()?;
            sample.AddBuffer(&MFCreateMemoryBuffer(self.out_bytes)?)?;
            Ok(sample)
        }
    }
}

unsafe fn hevc_writer(dst: &Path, size: (u32, u32), rate: u64, settings: EncodeSettings) -> Result<(IMFSinkWriter, u32)> {
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes: IMFAttributes = attributes.context("MFCreateAttributes returned nothing")?;
        attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        let url = HSTRING::from(std::path::absolute(dst)?.as_os_str());
        let writer = MFCreateSinkWriterFromURL(&url, None, &attributes).context("creating the MP4")?;

        let (numerator, denominator) = unpack(rate);
        let gop = (numerator * KEYFRAME_SECONDS).div_ceil(denominator.max(1));
        let encoded = video_type(&MFVideoFormat_HEVC, size, rate)?;
        encoded.SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate)?;
        let stream = writer.AddStream(&encoded).context("this PC has no HEVC encoder")?;
        writer.SetInputMediaType(stream, &nv12_type(size, rate)?, None).context("the HEVC encoder refuses NV12")?;

        let mut codec: Option<ICodecAPI> = None;
        let found = writer.GetServiceForStream(stream, &GUID::zeroed(), &ICodecAPI::IID, (&mut codec as *mut Option<ICodecAPI>).cast());
        if let (Ok(()), Some(codec)) = (found, codec) {
            codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(gop)).context("setting the keyframe interval")?;
            codec.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(settings.bitrate)).context("setting the bitrate")?;
        } else {
            bail!("the HEVC encoder offers no ICodecAPI, so the keyframe interval cannot be set");
        }
        Ok((writer, stream))
    }
}

unsafe fn open_reader(path: &Path, attributes: Option<&IMFAttributes>) -> Result<IMFSourceReader> {
    unsafe {
        let url = HSTRING::from(std::path::absolute(path).context("finding the video file")?.as_os_str());
        let reader = MFCreateSourceReaderFromURL(&url, attributes).with_context(|| format!("opening {}", path.display()))?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(VIDEO, true)?;
        Ok(reader)
    }
}

unsafe fn nv12_type(size: (u32, u32), rate: u64) -> Result<IMFMediaType> {
    unsafe { video_type(&MFVideoFormat_NV12, size, rate) }
}

unsafe fn video_type(subtype: &GUID, size: (u32, u32), rate: u64) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(size))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack((1, 1)))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(t)
    }
}

/// Encoders pad to whole macroblocks (1080 rows code as 1088); the display aperture holds the picture's real size.
unsafe fn visible_size(media_type: &IMFMediaType) -> Result<(u32, u32)> {
    unsafe {
        let mut area = MFVideoArea::default();
        let bytes = size_of::<MFVideoArea>() as u32;
        match media_type.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, std::slice::from_raw_parts_mut((&mut area as *mut MFVideoArea).cast(), bytes as usize), None) {
            Ok(()) => Ok((u32::try_from(area.Area.cx)?, u32::try_from(area.Area.cy)?)),
            Err(e) if e.code() == MF_E_ATTRIBUTENOTFOUND => Ok(unpack(media_type.GetUINT64(&MF_MT_FRAME_SIZE)?)),
            Err(e) => Err(e).context("reading the display aperture"),
        }
    }
}

fn pack((high, low): (u32, u32)) -> u64 {
    ((high as u64) << 32) | low as u64
}

fn unpack(value: u64) -> (u32, u32) {
    ((value >> 32) as u32, value as u32)
}

fn variant_u32(value: u32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: value },
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_aspect_keeps_everything() {
        assert_eq!(cover((3840, 2160), (1920, 1080)), RECT { left: 0, top: 0, right: 3840, bottom: 2160 });
    }

    #[test]
    fn ultrawide_loses_its_sides() {
        assert_eq!(cover((2560, 1080), (1920, 1080)), RECT { left: 320, top: 0, right: 2240, bottom: 1080 });
    }

    #[test]
    fn tall_video_loses_top_and_bottom_on_even_rows() {
        let r = cover((1080, 1920), (1920, 1080));
        assert_eq!((r.left, r.right), (0, 1080));
        assert_eq!(r.bottom - r.top, 606);
        assert_eq!(r.top % 2, 0);
    }
}
