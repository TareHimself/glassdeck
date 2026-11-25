use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use drm::buffer::{Buffer, DrmFourcc, DrmModifier, Handle as BufferHandle, PlanarBuffer};
use drm::control::atomic::AtomicModeReq;
use drm::control::dumbbuffer::DumbBuffer;
use drm::control::property::{Value, ValueType};
use drm::control::{
    AtomicCommitFlags, Device as ControlDevice, Event as DrmEvent, FbCmd2Flags, connector, crtc, framebuffer, plane,
    property,
};
use drm::{ClientCapability, Device};
use fontdue::Font;
use glassdeck_proto::unix_micros;
use rustix::fs::OFlags;
use rustix::time::{ClockId, clock_gettime};

use super::mpp::{Color, Encoding, Picture, Range};
use super::scanout::{Effect, Event, Scanout, step};
use crate::overlay::Overlay;
use crate::presentation::Outcome;

const CARD: &str = "/dev/dri/card0";
/// The keyboard's panel; a monitor offering this mode shows exactly what the keyboard would.
const PANEL_MODE: ((u16, u16), u32) = ((1920, 1080), 60);
const PLANE_TYPE_PRIMARY: u64 = 1;
/// Video goes under the labels; the primary plane (labels) is raised above it.
const VIDEO_ZPOS: u64 = 0;
const LABELS_ZPOS: u64 = 1;

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Device for Card {}
impl ControlDevice for Card {}

/// The key labels, drawn once by the CPU; transparent wherever there is no label.
struct Surface {
    _buffer: DumbBuffer,
    fb: framebuffer::Handle,
}

/// The video plane's properties, for atomic commits.
struct VideoPlane {
    handle: plane::Handle,
    fb_id: property::Handle,
    crtc_id: property::Handle,
    src: [property::Handle; 4],
    dst: [property::Handle; 4],
    color_encoding: property::Handle,
    color_range: property::Handle,
}

/// Drives the screen directly: decoded pictures are scanned out of the decoder's buffers by a
/// video plane, and the key labels sit on the primary plane above it, blended by the display controller.
/// Pictures go up with non-blocking atomic commits; the flip event says when each reached the screen.
pub struct Compositor {
    card: Card,
    crtc: crtc::Handle,
    video: VideoPlane,
    size: (u32, u32),
    applied_color: Option<Color>,
    scanout: Scanout,
    pictures: HashMap<u64, Picture>,
    framebuffers: HashMap<u64, framebuffer::Handle>,
    outcomes: Vec<Outcome>,
    _labels: Surface,
}

impl Compositor {
    pub fn open(font: Option<Font>) -> Result<Self> {
        // Non-blocking, so polling for flip events never stalls the compose loop.
        let card = Card(
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(OFlags::NONBLOCK.bits() as i32)
                .open(CARD)
                .with_context(|| format!("opening {CARD}"))?,
        );
        card.set_client_capability(ClientCapability::UniversalPlanes, true)
            .context("the display driver lacks universal planes")?;
        card.set_client_capability(ClientCapability::Atomic, true)
            .context("the display driver lacks atomic modesetting")?;
        let resources = card.resource_handles()?;

        let mut connected = None;
        for &handle in resources.connectors() {
            let info = card.get_connector(handle, true)?;
            if info.state() == connector::State::Connected {
                connected = Some(info);
                break;
            }
        }
        let connector = connected.context("no display is connected")?;
        let modes: Vec<_> = connector.modes().iter().map(|m| (m.size(), m.vrefresh())).collect();
        let mode = connector.modes()[pick_mode(&modes).context("the display offers no modes")?];
        let encoder_handle = connector
            .current_encoder()
            .or_else(|| connector.encoders().first().copied())
            .context("the connector has no encoder")?;
        let encoder = card.get_encoder(encoder_handle)?;
        let crtc = match encoder.crtc() {
            Some(crtc) => crtc,
            None => *resources.filter_crtcs(encoder.possible_crtcs()).first().context("no CRTC can drive the display")?,
        };
        let size = (mode.size().0 as u32, mode.size().1 as u32);

        let (video_plane, label_plane) = pick_planes(&card, &resources, crtc)?;
        card.set_property(video_plane, property_handle(&card, video_plane, "zpos")?, VIDEO_ZPOS)
            .context("moving the video plane down")?;
        card.set_property(label_plane, property_handle(&card, label_plane, "zpos")?, LABELS_ZPOS)
            .context("moving the label plane up")?;

        let image = match font {
            Some(font) => Overlay::build(&font, size.0 as usize, size.1 as usize).premultiplied_argb(),
            None => vec![0; size.0 as usize * size.1 as usize],
        };
        let labels = Surface::new(&card, size, &image)?;
        card.set_crtc(crtc, Some(labels.fb), (0, 0), &[connector.handle()], Some(mode))
            .context("setting the display mode")?;
        println!("Display: {}x{}@{} via CRTC {crtc:?}, video plane {video_plane:?}", size.0, size.1, mode.vrefresh());

        Ok(Self {
            video: VideoPlane::find(&card, video_plane)?,
            card,
            crtc,
            size,
            applied_color: None,
            scanout: Scanout::Steady { on_screen: None },
            pictures: HashMap::new(),
            framebuffers: HashMap::new(),
            outcomes: Vec::new(),
            _labels: labels,
        })
    }

    /// Hands over a picture without waiting; its fate comes back from `poll`.
    pub fn submit(&mut self, picture: Picture, token: u64) -> Result<()> {
        self.pictures.insert(token, picture);
        self.apply(Event::Submitted(token), 0)
    }

    /// Collects finished page flips and what became of every picture since the last call.
    pub fn poll(&mut self) -> Result<Vec<Outcome>> {
        loop {
            let events = match self.card.receive_events() {
                Ok(events) => events,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e).context("reading display events"),
            };
            for event in events {
                if let DrmEvent::PageFlip(flip) = event {
                    self.apply(Event::FlipDone, flip_wall_time_us(flip.duration))?;
                }
            }
        }
        Ok(std::mem::take(&mut self.outcomes))
    }

    fn apply(&mut self, event: Event, flipped_at_us: u64) -> Result<()> {
        let (next, effects) = step(self.scanout, event)?;
        self.scanout = next;
        for effect in effects {
            match effect {
                Effect::Commit(token) => self.commit(token)?,
                Effect::Presented(token) => self.outcomes.push(Outcome::Presented { token, at_us: flipped_at_us }),
                Effect::Release(token) => {
                    let fb = self.framebuffers.remove(&token).context("released a picture with no framebuffer")?;
                    self.card.destroy_framebuffer(fb)?;
                    self.pictures.remove(&token);
                }
                Effect::Drop(token) => {
                    self.pictures.remove(&token);
                    self.outcomes.push(Outcome::Dropped { token });
                }
            }
        }
        Ok(())
    }

    fn commit(&mut self, token: u64) -> Result<()> {
        let picture = self.pictures.get(&token).context("committing a picture that was never submitted")?;
        let (width, height) = picture.size();
        let (hor_stride, ver_stride) = picture.strides();
        let color = picture.color();
        let handle = self.card.prime_fd_to_buffer(picture.dma_buf()).context("importing the decoded picture")?;
        let nv12 = Nv12 { size: (width, height), handle, pitch: hor_stride, chroma_offset: hor_stride * ver_stride };
        let fb = self.card.add_planar_framebuffer(&nv12, FbCmd2Flags::empty());
        self.card.close_buffer(handle)?;
        let fb = fb.context("wrapping the decoded picture in a framebuffer")?;

        let video = &self.video;
        let mut request = AtomicModeReq::new();
        request.add_property(video.handle, video.fb_id, Value::Framebuffer(Some(fb)));
        request.add_property(video.handle, video.crtc_id, Value::CRTC(Some(self.crtc)));
        let (sx, sy, sw, sh) = whole((width, height));
        for (property, value) in video.src.iter().zip([sx, sy, sw, sh]) {
            request.add_property(video.handle, *property, Value::UnsignedRange(value as u64));
        }
        let (dx, dy, dw, dh) = fit((width, height), self.size);
        let dst = [
            Value::SignedRange(dx as i64),
            Value::SignedRange(dy as i64),
            Value::UnsignedRange(dw as u64),
            Value::UnsignedRange(dh as u64),
        ];
        for (property, value) in video.dst.iter().zip(dst) {
            request.add_property(video.handle, *property, value);
        }
        if self.applied_color != Some(color) {
            let (encoding, range) = color_names(color);
            let encoding = enum_value(&self.card, video.color_encoding, encoding)?;
            let range = enum_value(&self.card, video.color_range, range)?;
            request.add_property(video.handle, video.color_encoding, Value::Unknown(encoding));
            request.add_property(video.handle, video.color_range, Value::Unknown(range));
            println!("Video color: {color:?}");
            self.applied_color = Some(color);
        }
        self.card
            .atomic_commit(AtomicCommitFlags::NONBLOCK | AtomicCommitFlags::PAGE_FLIP_EVENT, request)
            .context("committing the decoded picture")?;
        self.framebuffers.insert(token, fb);
        Ok(())
    }

    pub fn idle(&mut self) {}

    pub fn is_open(&self) -> bool {
        true
    }

    pub fn set_status(&mut self, _status: &str) {}
}

impl Surface {
    fn new(card: &Card, size: (u32, u32), premultiplied_argb: &[u32]) -> Result<Self> {
        let mut buffer = card.create_dumb_buffer(size, DrmFourcc::Argb8888, 32).context("allocating the label buffer")?;
        let pitch = buffer.pitch() as usize;
        {
            let mut mapping = card.map_dumb_buffer(&mut buffer)?;
            for (row, pixels) in premultiplied_argb.chunks_exact(size.0 as usize).enumerate() {
                let dst = &mut mapping[row * pitch..][..pixels.len() * 4];
                for (bytes, pixel) in dst.chunks_exact_mut(4).zip(pixels) {
                    bytes.copy_from_slice(&pixel.to_le_bytes());
                }
            }
        }
        let fb = card.add_framebuffer(&buffer, 32, 32).context("wrapping the label buffer in a framebuffer")?;
        Ok(Self { _buffer: buffer, fb })
    }
}

struct Nv12 {
    size: (u32, u32),
    handle: BufferHandle,
    pitch: u32,
    chroma_offset: u32,
}

impl PlanarBuffer for Nv12 {
    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn format(&self) -> DrmFourcc {
        DrmFourcc::Nv12
    }

    fn modifier(&self) -> Option<DrmModifier> {
        None
    }

    fn pitches(&self) -> [u32; 4] {
        [self.pitch, self.pitch, 0, 0]
    }

    fn handles(&self) -> [Option<BufferHandle>; 4] {
        [Some(self.handle), Some(self.handle), None, None]
    }

    fn offsets(&self) -> [u32; 4] {
        [0, self.chroma_offset, 0, 0]
    }
}

/// Video needs a plane that scans out NV12; labels go on the primary plane, which takes linear ARGB.
/// (The Cluster planes are skipped: they only accept compressed buffers.)
fn pick_planes(card: &Card, resources: &drm::control::ResourceHandles, crtc: crtc::Handle) -> Result<(plane::Handle, plane::Handle)> {
    let mut video = None;
    let mut labels = None;
    for handle in card.plane_handles()? {
        let info = card.get_plane(handle)?;
        if !resources.filter_crtcs(info.possible_crtcs()).contains(&crtc) {
            continue;
        }
        let primary = property_value(card, handle, "type")? == PLANE_TYPE_PRIMARY;
        let formats = info.formats();
        if primary && formats.contains(&(DrmFourcc::Argb8888 as u32)) {
            labels.get_or_insert(handle);
        } else if !primary && formats.contains(&(DrmFourcc::Nv12 as u32)) {
            video.get_or_insert(handle);
        }
    }
    Ok((
        video.context("no plane can show NV12 video")?,
        labels.context("the primary plane cannot show ARGB labels")?,
    ))
}

impl VideoPlane {
    fn find(card: &Card, handle: plane::Handle) -> Result<Self> {
        let named = |name: &str| property_handle(card, handle, name);
        Ok(Self {
            handle,
            fb_id: named("FB_ID")?,
            crtc_id: named("CRTC_ID")?,
            src: [named("SRC_X")?, named("SRC_Y")?, named("SRC_W")?, named("SRC_H")?],
            dst: [named("CRTC_X")?, named("CRTC_Y")?, named("CRTC_W")?, named("CRTC_H")?],
            color_encoding: named("COLOR_ENCODING")?,
            color_range: named("COLOR_RANGE")?,
        })
    }
}

fn color_names(color: Color) -> (&'static str, &'static str) {
    let encoding = match color.encoding {
        Encoding::Bt601 => "ITU-R BT.601 YCbCr",
        Encoding::Bt709 => "ITU-R BT.709 YCbCr",
        Encoding::Bt2020 => "ITU-R BT.2020 YCbCr",
    };
    let range = match color.range {
        Range::Limited => "YCbCr limited range",
        Range::Full => "YCbCr full range",
    };
    (encoding, range)
}

/// Flip events carry CLOCK_MONOTONIC; latency figures use the wall clock.
fn flip_wall_time_us(flipped: Duration) -> u64 {
    let now = clock_gettime(ClockId::Monotonic);
    let now_monotonic_us = now.tv_sec as u64 * 1_000_000 + now.tv_nsec as u64 / 1_000;
    wall_time_us(flipped.as_micros() as u64, now_monotonic_us, unix_micros())
}

fn wall_time_us(event_monotonic_us: u64, now_monotonic_us: u64, now_wall_us: u64) -> u64 {
    now_wall_us.saturating_sub(now_monotonic_us.saturating_sub(event_monotonic_us))
}

fn property_handle(card: &Card, plane: plane::Handle, name: &str) -> Result<property::Handle> {
    let properties = card.get_properties(plane)?;
    let (handles, _) = properties.as_props_and_values();
    for &handle in handles {
        if card.get_property(handle)?.name().to_bytes() == name.as_bytes() {
            return Ok(handle);
        }
    }
    bail!("plane {plane:?} has no {name} property")
}

fn property_value(card: &Card, plane: plane::Handle, name: &str) -> Result<u64> {
    let properties = card.get_properties(plane)?;
    let (handles, values) = properties.as_props_and_values();
    for (&handle, &value) in handles.iter().zip(values) {
        if card.get_property(handle)?.name().to_bytes() == name.as_bytes() {
            return Ok(value);
        }
    }
    bail!("plane {plane:?} has no {name} property")
}

fn enum_value(card: &Card, property: property::Handle, name: &str) -> Result<u64> {
    let info = card.get_property(property)?;
    let ValueType::Enum(values) = info.value_type() else {
        bail!("{:?} is not an enum property", info.name());
    };
    let (raw, entries) = values.values();
    for (&raw, entry) in raw.iter().zip(entries) {
        if entry.name().to_bytes() == name.as_bytes() {
            return Ok(raw);
        }
    }
    bail!("{:?} has no {name} option", info.name())
}

fn pick_mode(modes: &[((u16, u16), u32)]) -> Option<usize> {
    modes.iter().position(|&m| m == PANEL_MODE).or(if modes.is_empty() { None } else { Some(0) })
}

fn whole(size: (u32, u32)) -> (u32, u32, u32, u32) {
    (0, 0, size.0 << 16, size.1 << 16)
}

/// Largest rectangle with the source's aspect ratio, centred in the destination.
fn fit(src: (u32, u32), dst: (u32, u32)) -> (i32, i32, u32, u32) {
    let (sw, sh, dw, dh) = (src.0 as u64, src.1 as u64, dst.0 as u64, dst.1 as u64);
    let (w, h) = if sw * dh >= sh * dw { (dw, sh * dw / sw) } else { (sw * dh / sh, dh) };
    (((dw - w) / 2) as i32, ((dh - h) / 2) as i32, w as u32, h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_time_moves_onto_the_wall_clock() {
        assert_eq!(wall_time_us(1_000, 5_000, 1_000_000), 996_000);
    }

    #[test]
    fn prefers_the_panel_mode() {
        assert_eq!(pick_mode(&[((2560, 1440), 60), ((1920, 1080), 120), ((1920, 1080), 60)]), Some(2));
    }

    #[test]
    fn falls_back_to_the_preferred_mode() {
        assert_eq!(pick_mode(&[((2560, 1440), 60), ((1280, 720), 60)]), Some(0));
        assert_eq!(pick_mode(&[]), None);
    }

    #[test]
    fn same_size_fills_the_screen() {
        assert_eq!(fit((1920, 1080), (1920, 1080)), (0, 0, 1920, 1080));
    }

    #[test]
    fn wider_source_is_letterboxed() {
        assert_eq!(fit((1920, 800), (1920, 1080)), (0, 140, 1920, 800));
    }

    #[test]
    fn taller_source_is_pillarboxed() {
        assert_eq!(fit((1080, 1080), (1920, 1080)), (420, 0, 1080, 1080));
    }

    #[test]
    fn smaller_source_is_scaled_up() {
        assert_eq!(fit((1280, 720), (1920, 1080)), (0, 0, 1920, 1080));
    }
}
