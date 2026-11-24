use std::mem::ManuallyDrop;

use anyhow::{Context, Result, anyhow, bail};
use glassdeck_proto::Codec;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVLowLatencyMode, ICodecAPI, IMFActivate, IMFMediaType, IMFTransform,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_MT_YUV_MATRIX, MF_VERSION, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSTARTUP_FULL, MFStartup,
    MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_LOCALMFT,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_FRIENDLY_NAME_Attribute,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264,
    MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoTransferMatrix_BT601,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};
use windows::core::{Interface, PWSTR};

use crate::yuv::{Matrix, Nv12, nv12_to_xrgb};

const HUNDRED_NS_PER_US: i64 = 10;

pub struct Picture {
    pub pixels: Vec<u32>,
    pub width: usize,
    pub height: usize,
    pub time_us: u64,
}

struct OutputFormat {
    pitch: usize,
    coded_width: usize,
    coded_height: usize,
    matrix: Matrix,
    sample_size: u32,
}

/// Windows' own H.264/HEVC decoder; unlike openh264 it handles the profiles real files use (High, Main10).
pub struct Decoder {
    transform: IMFTransform,
    output: OutputFormat,
    provides_samples: bool,
    visible: (usize, usize),
    pub name: String,
}

pub fn init_media_foundation() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    Ok(())
}

impl Decoder {
    pub fn new(codec: Codec, width: usize, height: usize) -> Result<Self> {
        unsafe {
            let (activate, name) = find_decoder(codec)?;
            let transform: IMFTransform = activate.ActivateObject()?;
            transform
                .cast::<ICodecAPI>()?
                .SetValue(&CODECAPI_AVLowLatencyMode, &variant_u32(1))
                .context("decoder refused low-latency mode")?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &subtype(codec))?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (width as u64) << 32 | height as u64)?;
            transform.SetInputType(0, &input, 0).context("decoder refused the input type")?;
            let output = select_nv12_output(&transform)?;

            let info = transform.GetOutputStreamInfo(0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(Self {
                transform,
                output,
                provides_samples: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                visible: (width, height),
                name,
            })
        }
    }

    /// Feeds one access unit and returns every picture the decoder has ready, in display order.
    pub fn decode(&mut self, access_unit: &[u8], time_us: u64) -> Result<Vec<Picture>> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(access_unit.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(access_unit.as_ptr(), ptr, access_unit.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(access_unit.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time_us as i64 * HUNDRED_NS_PER_US)?;
            self.transform.ProcessInput(0, &sample, 0).context("decoder refused input")?;
        }

        let mut pictures = Vec::new();
        while let Some(picture) = self.next_picture()? {
            pictures.push(picture);
        }
        Ok(pictures)
    }

    fn next_picture(&mut self) -> Result<Option<Picture>> {
        unsafe {
            let supplied = if self.provides_samples {
                None
            } else {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(self.output.sample_size)?)?;
                Some(sample)
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(supplied),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let result = self.transform.ProcessOutput(0, &mut buffers, &mut status);
            let sample = ManuallyDrop::take(&mut buffers[0].pSample);
            ManuallyDrop::drop(&mut buffers[0].pEvents);

            match result {
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.output = select_nv12_output(&self.transform)?;
                    return self.next_picture();
                }
                other => other.context("decoder failed")?,
            }

            let sample = sample.context("decoder produced no sample")?;
            let time_us = (sample.GetSampleTime()? / HUNDRED_NS_PER_US).max(0) as u64;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut len = 0;
            buffer.Lock(&mut data, None, Some(&mut len))?;
            let format = &self.output;
            let luma_len = format.pitch * format.coded_height;
            let needed = luma_len + format.pitch * format.coded_height / 2;
            if (len as usize) < needed {
                buffer.Unlock()?;
                bail!("decoder returned {len} bytes, expected at least {needed}");
            }
            let plane = std::slice::from_raw_parts(data, needed);

            let width = self.visible.0.min(format.coded_width);
            let height = self.visible.1.min(format.coded_height);
            let picture = Nv12 { luma: &plane[..luma_len], chroma: &plane[luma_len..], width, height, pitch: format.pitch };
            let mut pixels = vec![0u32; width * height];
            nv12_to_xrgb(&picture, format.matrix, &mut pixels);
            buffer.Unlock()?;

            Ok(Some(Picture { pixels, width, height, time_us }))
        }
    }
}

fn subtype(codec: Codec) -> windows::core::GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
    }
}

unsafe fn select_nv12_output(transform: &IMFTransform) -> Result<OutputFormat> {
    unsafe {
        for index in 0.. {
            let candidate: IMFMediaType = match transform.GetOutputAvailableType(0, index) {
                Ok(t) => t,
                Err(_) => break,
            };
            if candidate.GetGUID(&MF_MT_SUBTYPE)? != MFVideoFormat_NV12 {
                continue;
            }
            transform.SetOutputType(0, &candidate, 0).context("decoder refused NV12 output")?;
            let size = candidate.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let matrix = match candidate.GetUINT32(&MF_MT_YUV_MATRIX) {
                Ok(m) if m == MFVideoTransferMatrix_BT601.0 as u32 => Matrix::Bt601,
                _ => Matrix::Bt709,
            };
            let coded_width = (size >> 32) as usize;
            let pitch = match candidate.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
                Ok(stride) => stride as i32 as isize as usize,
                Err(_) => coded_width, // Media Foundation's documented default stride for NV12
            };
            return Ok(OutputFormat {
                pitch,
                coded_width,
                coded_height: (size & 0xFFFF_FFFF) as usize,
                matrix,
                sample_size: transform.GetOutputStreamInfo(0)?.cbSize.max(1),
            });
        }
        bail!("decoder offers no NV12 output")
    }
}

unsafe fn find_decoder(codec: Codec) -> Result<(IMFActivate, String)> {
    unsafe {
        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype(codec) };
        let mut list = std::ptr::null_mut();
        let mut count = 0;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            None,
            &mut list,
            &mut count,
        )?;
        let activates: Vec<Option<IMFActivate>> = (0..count as usize).map(|i| std::ptr::read(list.add(i))).collect();
        CoTaskMemFree(Some(list as *const _));
        let activate = activates.into_iter().flatten().next().ok_or_else(|| match codec {
            Codec::H264 => anyhow!("Windows has no H.264 decoder installed"),
            Codec::Hevc => anyhow!("Windows has no HEVC decoder; install HEVC Video Extensions from the Microsoft Store"),
        })?;

        let mut name = PWSTR::null();
        let mut len = 0;
        activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut len)?;
        let text = name.to_string()?;
        CoTaskMemFree(Some(name.0 as *const _));
        Ok((activate, text))
    }
}

/// The decoder takes low-latency mode as VT_UI4, while the encoder takes VT_BOOL.
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
