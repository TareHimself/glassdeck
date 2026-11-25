use std::sync::mpsc::SyncSender;
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod,
    RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use windows_capture::frame::Frame;

use glassdeck_proto::Codec;

use crate::stream::{EncodedFrame, EncoderSettings};

pub struct SoftwareEncoder {
    output: SyncSender<EncodedFrame>,
    encoder: Encoder,
    yuv: YUVBuffer,
    scratch: Vec<u8>,
    size: (usize, usize),
    seq: u32,
}

impl SoftwareEncoder {
    pub fn new(settings: EncoderSettings, output: SyncSender<EncodedFrame>) -> Result<Self> {
        if settings.codec != Codec::H264 {
            bail!("openh264 only encodes H.264; use --codec h264 or the GPU encoder");
        }
        if settings.vbv_frames.is_some() {
            bail!("--vbv-frames needs the GPU encoder");
        }
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(settings.bitrate))
            .max_frame_rate(FrameRate::from_hz(settings.fps as f32))
            .skip_frames(false)
            .complexity(Complexity::Low)
            .intra_frame_period(IntraFramePeriod::from_num_frames(settings.keyframe_every));
        let size = (settings.width as usize, settings.height as usize);
        Ok(Self {
            output,
            encoder: Encoder::with_api_config(OpenH264API::from_source(), config)?,
            yuv: YUVBuffer::new(size.0, size.1),
            scratch: Vec::new(),
            size,
            seq: 0,
        })
    }

    pub fn submit(&mut self, frame: &mut Frame, capture_us: u64) -> Result<()> {
        let started = Instant::now();
        let buffer = frame.buffer()?;
        let bgra = buffer.as_nopadding_buffer(&mut self.scratch);
        self.yuv.read_bgra8(BgraSliceU8::new(bgra, self.size));
        let bitstream = self.encoder.encode(&self.yuv)?;
        let keyframe = matches!(bitstream.frame_type(), FrameType::IDR | FrameType::I);
        let data = bitstream.to_vec();
        if data.is_empty() {
            return Ok(());
        }

        let encoded = EncodedFrame { seq: self.seq, capture_us, keyframe, data, encode_time: started.elapsed() };
        self.output.send(encoded).map_err(|_| anyhow!("network thread stopped"))?;
        self.seq += 1;
        Ok(())
    }
}
