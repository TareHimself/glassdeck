use std::mem::ManuallyDrop;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use glassdeck_proto::{Codec, unix_micros};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonBufferSize, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVLowLatencyMode,
    ICodecAPI, IMF2DBuffer, IMFActivate, IMFAttributes, IMFDXGIDeviceManager,
    IMFMediaEventGenerator, IMFMediaType, IMFTransform, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    METransformHaveOutput, METransformNeedInput, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_TYPE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
    MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSTARTUP_FULL,
    MFSampleExtension_CleanPoint, MFStartup, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnum2, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12,
    MFNominalRange_16_235, MFVideoInterlace_Progressive, MFVideoTransferMatrix_BT709, eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_High, eAVEncH265VProfile_Main_420_8,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::Foundation::{VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::System::Variant::{VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4};
use windows::core::{GUID, Interface, PWSTR};

use crate::desktop::{EncodedFrame, EncoderSettings};

const QUEUED_FRAMES: usize = 2;
const HUNDRED_NS_PER_US: u64 = 10;
/// D3D11_VIDEO_PROCESSOR_COLOR_SPACE bits: 1 = RGB_Range (limited), 2 = YCbCr_Matrix (BT.709), 4-5 = Nominal_Range.
const FULL_RANGE_RGB: D3D11_VIDEO_PROCESSOR_COLOR_SPACE = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
const BT709_LIMITED_YUV: D3D11_VIDEO_PROCESSOR_COLOR_SPACE = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
    _bitfield: 1 << 2 | (D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235.0 as u32) << 4,
};
const H264_PROFILE: u32 = eAVEncH264VProfile_High.0 as u32;
const HEVC_PROFILE: u32 = eAVEncH265VProfile_Main_420_8.0 as u32;

fn subtype_and_profile(codec: Codec) -> (GUID, u32) {
    match codec {
        Codec::H264 => (MFVideoFormat_H264, H264_PROFILE),
        Codec::Hevc => (MFVideoFormat_HEVC, HEVC_PROFILE),
    }
}

struct GpuFrame {
    texture: ID3D11Texture2D,
    capture_us: u64,
}

struct SendDevice(ID3D11Device);

// SAFETY: D3D11 devices and resources are free-threaded, and we turn on multithread protection.
unsafe impl Send for GpuFrame {}
unsafe impl Send for SendDevice {}

pub struct GpuEncoder {
    converter: Nv12Converter,
    input: SyncSender<GpuFrame>,
    thread: Option<JoinHandle<Result<()>>>,
    pub name: String,
}

impl GpuEncoder {
    pub fn start(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        config: EncoderSettings,
        output: SyncSender<EncodedFrame>,
    ) -> Result<Self> {
        unsafe {
            let _ = context.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
        }
        let converter = Nv12Converter::new(device, context, config)?;
        let (input, input_rx) = mpsc::sync_channel(QUEUED_FRAMES);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let shared = SendDevice(device.clone());

        let thread = thread::Builder::new()
            .name("gpu-encoder".into())
            .spawn(move || encoder_thread(shared, config, input_rx, output, ready_tx))?;
        let name = ready_rx.recv().context("encoder thread exited during setup")??;
        Ok(Self { converter, input, thread: Some(thread), name })
    }

    pub fn submit(&mut self, bgra: &ID3D11Texture2D, capture_us: u64) -> Result<()> {
        let texture = self.converter.convert(bgra)?;
        if self.input.send(GpuFrame { texture, capture_us }).is_err() {
            return Err(self.thread_failure());
        }
        Ok(())
    }

    fn thread_failure(&mut self) -> anyhow::Error {
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(Err(e))) => e.context("GPU encoder failed"),
            Some(Err(_)) => anyhow!("GPU encoder thread panicked"),
            _ => anyhow!("GPU encoder stopped"),
        }
    }
}

struct Nv12Converter {
    device: ID3D11Device,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    width: u32,
    height: u32,
}

impl Nv12Converter {
    fn new(device: &ID3D11Device, context: &ID3D11DeviceContext, config: EncoderSettings) -> Result<Self> {
        let video_device: ID3D11VideoDevice = device.cast().context("GPU device has no video support")?;
        let video_context: ID3D11VideoContext = context.cast()?;
        let rate = DXGI_RATIONAL { Numerator: config.fps, Denominator: 1 };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: config.width,
            InputHeight: config.height,
            OutputFrameRate: rate,
            OutputWidth: config.width,
            OutputHeight: config.height,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let (enumerator, processor) = unsafe {
            let enumerator = video_device.CreateVideoProcessorEnumerator(&desc)?;
            let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &FULL_RANGE_RGB);
            video_context.VideoProcessorSetOutputColorSpace(&processor, &BT709_LIMITED_YUV);
            (enumerator, processor)
        };
        Ok(Self {
            device: device.clone(),
            video_device,
            video_context,
            enumerator,
            processor,
            width: config.width,
            height: config.height,
        })
    }

    fn convert(&self, bgra: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width,
            Height: self.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };

        unsafe {
            let mut nv12 = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut nv12))?;
            let nv12 = nv12.context("CreateTexture2D returned nothing")?;

            let mut input_view = None;
            self.video_device
                .CreateVideoProcessorInputView(bgra, &self.enumerator, &input_desc, Some(&mut input_view))
                .context("creating video processor input view")?;
            let mut output_view = None;
            self.video_device
                .CreateVideoProcessorOutputView(&nv12, &self.enumerator, &output_desc, Some(&mut output_view))
                .context("creating video processor output view")?;

            let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(input_view),
                ..Default::default()
            }];
            let blt = self.video_context.VideoProcessorBlt(
                &self.processor,
                output_view.as_ref().context("output view missing")?,
                0,
                &streams,
            );
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            blt.context("converting BGRA to NV12")?;
            Ok(nv12)
        }
    }
}

fn encoder_thread(
    device: SendDevice,
    config: EncoderSettings,
    input: Receiver<GpuFrame>,
    output: SyncSender<EncodedFrame>,
    ready: SyncSender<Result<String>>,
) -> Result<()> {
    let mut encoder = match unsafe { HardwareEncoder::open(&device.0, config) } {
        Ok(encoder) => encoder,
        Err(e) => {
            let _ = ready.send(Err(e));
            return Ok(());
        }
    };
    let _ = ready.send(Ok(encoder.name.clone()));
    unsafe { encoder.run(input, output) }
}

struct HardwareEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    _manager: IMFDXGIDeviceManager,
    name: String,
    provides_samples: bool,
    output_buffer_size: u32,
    frame_duration: i64,
    first_capture_us: Option<u64>,
    seq: u32,
}

impl HardwareEncoder {
    unsafe fn open(device: &ID3D11Device, config: EncoderSettings) -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

            let (activate, name) = find_hardware_encoder(device, config.codec)?;
            let transform: IMFTransform = activate.ActivateObject()?;
            transform.GetAttributes()?.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;

            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            configure_codec(&transform.cast()?, config)?;
            transform.SetOutputType(0, &output_type(config)?, 0).context("setting H.264 output type")?;
            transform.SetInputType(0, &input_type(config)?, 0).context("setting NV12 input type")?;

            let info = transform.GetOutputStreamInfo(0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

            Ok(Self {
                events: transform.cast()?,
                transform,
                _manager: manager,
                name,
                provides_samples: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                output_buffer_size: info.cbSize.max(1),
                frame_duration: 10_000_000 / config.fps as i64,
                first_capture_us: None,
                seq: 0,
            })
        }
    }

    unsafe fn run(&mut self, input: Receiver<GpuFrame>, output: SyncSender<EncodedFrame>) -> Result<()> {
        loop {
            let event = unsafe { self.events.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))? };
            unsafe { event.GetStatus()?.ok().context("encoder reported an error event")? };
            match MF_EVENT_TYPE(unsafe { event.GetType()? } as i32) {
                t if t == METransformNeedInput => match input.recv() {
                    Ok(frame) => unsafe { self.feed(frame)? },
                    Err(_) => return Ok(()),
                },
                t if t == METransformHaveOutput => {
                    if let Some(encoded) = unsafe { self.drain()? } {
                        output.send(encoded).map_err(|_| anyhow!("network thread stopped"))?;
                    }
                }
                _ => {}
            }
        }
    }

    unsafe fn feed(&mut self, frame: GpuFrame) -> Result<()> {
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &frame.texture, 0, false)?;
            let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buffer.SetCurrentLength(length)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;

            let first = *self.first_capture_us.get_or_insert(frame.capture_us);
            sample.SetSampleTime(((frame.capture_us - first) * HUNDRED_NS_PER_US) as i64)?;
            sample.SetSampleDuration(self.frame_duration)?;
            self.transform.ProcessInput(0, &sample, 0)?;
        }
        Ok(())
    }

    unsafe fn drain(&mut self) -> Result<Option<EncodedFrame>> {
        unsafe {
            let supplied = if self.provides_samples {
                None
            } else {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(self.output_buffer_size)?)?;
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
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    let negotiated = self.transform.GetOutputAvailableType(0, 0)?;
                    self.transform.SetOutputType(0, &negotiated, 0)?;
                    return Ok(None);
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                other => other.context("ProcessOutput")?,
            }

            let sample = sample.context("encoder produced no sample")?;
            let time = sample.GetSampleTime()?;
            let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).is_ok_and(|v| v != 0);

            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;

            let first = self.first_capture_us.context("output before any input")?;
            let capture_us = first + time as u64 / HUNDRED_NS_PER_US;
            let seq = self.seq;
            self.seq += 1;
            Ok(Some(EncodedFrame {
                seq,
                capture_us,
                keyframe,
                data,
                encode_time: Duration::from_micros(unix_micros().saturating_sub(capture_us)),
            }))
        }
    }
}

unsafe fn find_hardware_encoder(device: &ID3D11Device, codec: Codec) -> Result<(IMFActivate, String)> {
    unsafe {
        let luid = device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?.AdapterLuid;
        let mut attributes: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.context("no attributes")?;
        let luid_bytes = std::slice::from_raw_parts((&luid as *const _) as *const u8, size_of_val(&luid));
        attributes.SetBlob(&MFT_ENUM_ADAPTER_LUID, luid_bytes)?;

        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
        let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype_and_profile(codec).0 };
        let mut list = std::ptr::null_mut();
        let mut count = 0;
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &attributes,
            &mut list,
            &mut count,
        )?;
        let activates: Vec<Option<IMFActivate>> =
            (0..count as usize).map(|i| std::ptr::read(list.add(i))).collect();
        CoTaskMemFree(Some(list as *const _));

        let activate = activates.into_iter().flatten().next();
        let Some(activate) = activate else {
            bail!("this GPU has no hardware {codec:?} encoder");
        };
        Ok((activate.clone(), friendly_name(&activate)?))
    }
}

unsafe fn friendly_name(activate: &IMFActivate) -> Result<String> {
    unsafe {
        let mut name = PWSTR::null();
        let mut len = 0;
        activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut len)?;
        let text = name.to_string()?;
        CoTaskMemFree(Some(name.0 as *const _));
        Ok(text)
    }
}

unsafe fn configure_codec(codec: &ICodecAPI, config: EncoderSettings) -> Result<()> {
    unsafe {
        codec
            .SetValue(&CODECAPI_AVLowLatencyMode, &variant_bool(true))
            .context("encoder refused low-latency mode")?;
        codec
            .SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(config.keyframe_every))
            .context("encoder refused the keyframe interval")?;
        if let Some(frames) = config.vbv_frames {
            codec
                .SetValue(&CODECAPI_AVEncCommonBufferSize, &variant_u32(vbv_bits(config, frames)))
                .context("encoder refused the rate-control buffer size")?;
        }
        let tuning: [(&GUID, u32, &str); 2] = [
            (&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32, "CBR"),
            (&CODECAPI_AVEncCommonMeanBitRate, config.bitrate, "bitrate"),
        ];
        for (key, value, what) in tuning {
            if let Err(e) = codec.SetValue(key, &variant_u32(value)) {
                eprintln!("warning: encoder ignored {what}: {e}");
            }
        }
    }
    Ok(())
}

fn vbv_bits(config: EncoderSettings, frames: u32) -> u32 {
    (config.bitrate as u64 * frames as u64 / config.fps.max(1) as u64) as u32
}

fn variant(vt: VARENUM, value: VARIANT_0_0_0) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: value,
            }),
        },
    }
}

fn variant_u32(value: u32) -> VARIANT {
    variant(VT_UI4, VARIANT_0_0_0 { ulVal: value })
}

fn variant_bool(value: bool) -> VARIANT {
    let flag = if value { VARIANT_TRUE } else { VARIANT_FALSE };
    variant(VT_BOOL, VARIANT_0_0_0 { boolVal: flag })
}

fn pack(high: u32, low: u32) -> u64 {
    (high as u64) << 32 | low as u64
}

unsafe fn output_type(config: EncoderSettings) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        let (subtype, profile) = subtype_and_profile(config.codec);
        t.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(config.width, config.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(config.fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_MPEG2_PROFILE, profile)?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(t)
    }
}

unsafe fn input_type(config: EncoderSettings) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(config.width, config.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(config.fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
        D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_USAGE_STAGING, D3D11CreateDevice,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

    const SIZE: u32 = 16;

    fn device() -> (ID3D11Device, ID3D11DeviceContext) {
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .unwrap();
        }
        (device.unwrap(), context.unwrap())
    }

    fn solid_bgra(device: &ID3D11Device, bgra: [u8; 4]) -> ID3D11Texture2D {
        let pixels: Vec<u8> = bgra.iter().copied().cycle().take((SIZE * SIZE * 4) as usize).collect();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: SIZE,
            Height: SIZE,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr().cast(), SysMemPitch: SIZE * 4, SysMemSlicePitch: 0 };
        let mut texture = None;
        unsafe { device.CreateTexture2D(&desc, Some(&data), Some(&mut texture)).unwrap() };
        texture.unwrap()
    }

    fn read_yuv(device: &ID3D11Device, context: &ID3D11DeviceContext, nv12: &ID3D11Texture2D) -> (u8, u8, u8) {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { nv12.GetDesc(&mut desc) };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        let mut staging = None;
        unsafe {
            device.CreateTexture2D(&desc, None, Some(&mut staging)).unwrap();
            let staging = staging.unwrap();
            context.CopyResource(&staging, nv12);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).unwrap();
            let base = mapped.pData as *const u8;
            let pitch = mapped.RowPitch as usize;
            let y = *base.add(pitch * 8 + 8);
            let chroma = base.add(pitch * SIZE as usize + pitch * 4 + 8);
            let yuv = (y, *chroma, *chroma.add(1));
            context.Unmap(&staging, 0);
            yuv
        }
    }

    fn assert_close(got: (u8, u8, u8), expected: (u8, u8, u8), what: &str) {
        let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 2;
        assert!(
            near(got.0, expected.0) && near(got.1, expected.1) && near(got.2, expected.2),
            "{what}: got YUV {got:?}, expected {expected:?}"
        );
    }

    #[test]
    #[ignore = "needs a GPU; run with cargo test -- --ignored"]
    fn converter_produces_bt709_limited_range() {
        let (device, context) = device();
        let settings = EncoderSettings {
            width: SIZE,
            height: SIZE,
            codec: Codec::H264,
            fps: 60,
            bitrate: 1_000_000,
            keyframe_every: 60,
            vbv_frames: None,
        };
        let converter = Nv12Converter::new(&device, &context, settings).unwrap();
        let cases = [
            ([0, 0, 0, 255], (16, 128, 128), "black"),
            ([255, 255, 255, 255], (235, 128, 128), "white"),
            ([0, 0, 255, 255], (63, 102, 240), "red"),
            ([0, 255, 0, 255], (173, 42, 26), "green"),
            ([255, 0, 0, 255], (32, 240, 118), "blue"),
        ];
        for (bgra, expected, what) in cases {
            let nv12 = converter.convert(&solid_bgra(&device, bgra)).unwrap();
            assert_close(read_yuv(&device, &context, &nv12), expected, what);
        }
    }

    #[test]
    fn packs_size_as_high_and_low_words() {
        assert_eq!(pack(1920, 1080), 0x0000_0780_0000_0438);
    }

    #[test]
    fn vbv_of_one_frame_is_one_average_frame_of_bits() {
        let config = EncoderSettings {
            width: 1920,
            height: 1080,
            codec: Codec::H264,
            fps: 60,
            bitrate: 15_000_000,
            keyframe_every: 120,
            vbv_frames: Some(1),
        };
        assert_eq!(vbv_bits(config, 1), 250_000);
        assert_eq!(vbv_bits(config, 4), 1_000_000);
    }
}
