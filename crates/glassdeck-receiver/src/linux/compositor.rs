use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use drm::buffer::{Buffer, DrmFourcc, DrmModifier, Handle as BufferHandle, PlanarBuffer};
use drm::control::atomic::AtomicModeReq;

use crate::PanelRotation;
use drm::control::dumbbuffer::DumbBuffer;
use drm::control::property::{Value, ValueType};
use drm::control::{
    AtomicCommitFlags, Device as ControlDevice, Event as DrmEvent, FbCmd2Flags, connector, crtc, framebuffer, plane,
    property,
};
use drm::{ClientCapability, Device};
use glassdeck_proto::compositor::BufferLayout;
use glassdeck_proto::unix_micros;
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::OFlags;
use rustix::io::Errno;
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
use rustix::time::{ClockId, clock_gettime};

use super::{Doorbell, Picture};
use super::mpp::{Color, Encoding, Range};
use super::scanout::{Effect, Event, Scanout, Update, step};
use crate::overlay::{Labels, Overlay};
use crate::presentation::Outcome;
use crate::widgets::Tile;

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

/// ARGB8888 is stored little-endian, so a premultiplied ARGB word copies into the buffer as it is.
const _ARGB_WORDS_MATCH_THE_BUFFER: () = assert!(cfg!(target_endian = "little"));

/// One of the key layer's two buffers, mapped for its whole life.
struct Surface {
    _buffer: DumbBuffer,
    fb: framebuffer::Handle,
    pixels: Mapped,
    /// Words per row
    stride: usize,
}

/// A dumb buffer's memory, mapped once and unmapped on drop.
struct Mapped {
    words: NonNull<u32>,
    len: usize,
}

impl Mapped {
    fn new(card: &Card, buffer: &DumbBuffer) -> Result<Self> {
        let info = drm_ffi::mode::dumbbuffer::map(card.as_fd(), buffer.handle().into(), 0, 0).context("finding the key layer's memory")?;
        let bytes = buffer.pitch() as usize * buffer.size().1 as usize;
        let prot = ProtFlags::READ | ProtFlags::WRITE;
        // SAFETY: a fresh shared mapping of the buffer, which nothing else maps; it's unmapped in Drop.
        let map = unsafe { mmap(std::ptr::null_mut(), bytes, prot, MapFlags::SHARED, card.as_fd(), info.offset) }
            .context("mapping the key layer")?;
        let words = NonNull::new(map.cast::<u32>()).context("the key layer mapped at null")?;
        Ok(Self { words, len: bytes / 4 })
    }

    fn words(&mut self) -> &mut [u32] {
        // SAFETY: the mapping is page-aligned, `len` words long, and only reached through `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.words.as_ptr(), self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly what `new` mapped, once.
        if let Err(e) = unsafe { munmap(self.words.as_ptr().cast(), self.len * 4) } {
            eprintln!("error: unmapping the key layer: {e}");
        }
    }
}

/// A rectangle of the panel, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Region {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// The key layer: labels plus widget tiles, double-buffered so a frame is never drawn into the
/// buffer on screen. `mirror` holds the latest content; each buffer lists the regions it lacks.
struct KeyLayer {
    plane: plane::Handle,
    fb_id: property::Handle,
    buffers: [Surface; 2],
    front: usize,
    mirror: Vec<u32>,
    width: u32,
    stale: [Vec<Region>; 2],
    /// The primary plane can't turn, so labels are drawn already turned when the panel is.
    turned: bool,
}

impl KeyLayer {
    fn write(&mut self, tiles: Vec<Tile>) -> Result<()> {
        let height = self.mirror.len() as u32 / self.width;
        for tile in tiles {
            let region = Region { x: tile.x, y: tile.y, w: tile.w, h: tile.h };
            ensure!(
                region.x + region.w <= self.width && region.y + region.h <= height && tile.pixels.len() == (region.w * region.h) as usize,
                "a widget tile at {region:?} doesn't fit the {}x{height} key layer",
                self.width
            );
            let (region, pixels) = if self.turned {
                // A half turn of a tile is its pixels in reverse, at the mirrored position.
                let turned = Region { x: self.width - region.x - region.w, y: height - region.y - region.h, ..region };
                (turned, tile.pixels.iter().rev().copied().collect())
            } else {
                (region, tile.pixels)
            };
            for (row, pixels) in pixels.chunks_exact(region.w as usize).enumerate() {
                let at = (region.y as usize + row) * self.width as usize + region.x as usize;
                self.mirror[at..at + pixels.len()].copy_from_slice(pixels);
            }
            for stale in &mut self.stale {
                stale.push(region);
            }
        }
        Ok(())
    }

    /// Brings the back buffer up to date and returns its framebuffer, to be committed.
    fn prepare_back(&mut self) -> framebuffer::Handle {
        let back = 1 - self.front;
        let regions = std::mem::take(&mut self.stale[back]);
        let surface = &mut self.buffers[back];
        let stride = surface.stride;
        let words = surface.pixels.words();
        for region in regions {
            for row in region.y as usize..(region.y + region.h) as usize {
                let (x, w) = (region.x as usize, region.w as usize);
                let from = row * self.width as usize + x;
                words[row * stride + x..][..w].copy_from_slice(&self.mirror[from..from + w]);
            }
        }
        surface.fb
    }
}

/// The video plane's properties, for atomic commits.
#[derive(Clone)]
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
    card: Arc<Card>,
    crtc: crtc::Handle,
    video: VideoPlane,
    size: (u32, u32),
    applied_color: Option<Color>,
    scanout: Scanout,
    pictures: HashMap<u64, Picture>,
    /// Framebuffers made for decoded pictures; shared buffers keep theirs for their whole life.
    framebuffers: HashMap<u64, framebuffer::Handle>,
    outcomes: Vec<Outcome>,
    keys: KeyLayer,
    doorbell: Arc<Doorbell>,
    rotation: PanelRotation,
}

impl Compositor {
    pub fn open(labels: Option<Labels>, rotation: PanelRotation) -> Result<Self> {
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

        let turned = rotation == PanelRotation::Turned180;
        let mut image = match labels {
            Some(labels) => Overlay::build(&labels, size.0 as usize, size.1 as usize).premultiplied_argb(),
            None => vec![0; size.0 as usize * size.1 as usize],
        };
        if turned {
            image.reverse();
        }
        let buffers = [Surface::new(&card, size, &image)?, Surface::new(&card, size, &image)?];
        card.set_crtc(crtc, Some(buffers[0].fb), (0, 0), &[connector.handle()], Some(mode))
            .context("setting the display mode")?;
        let keys = KeyLayer {
            plane: label_plane,
            fb_id: property_handle(&card, label_plane, "FB_ID")?,
            buffers,
            front: 0,
            mirror: image,
            width: size.0,
            stale: [Vec::new(), Vec::new()],
            turned,
        };
        println!("Display: {}x{}@{} via CRTC {crtc:?}, video plane {video_plane:?}", size.0, size.1, mode.vrefresh());

        Ok(Self {
            video: VideoPlane::find(&card, video_plane)?,
            card: Arc::new(card),
            crtc,
            size,
            applied_color: None,
            scanout: Scanout::Steady { on_screen: None },
            pictures: HashMap::new(),
            framebuffers: HashMap::new(),
            outcomes: Vec::new(),
            keys,
            doorbell: Arc::new(Doorbell::new()?),
            rotation,
        })
    }

    /// Hands over a picture without waiting; its fate comes back from `poll`.
    pub fn submit(&mut self, picture: Picture, token: u64) -> Result<()> {
        self.pictures.insert(token, picture);
        self.apply(Event::Submitted(token), 0)
    }

    /// Puts widget tiles on the key layer at the next commit.
    pub fn show_tiles(&mut self, tiles: Vec<Tile>) -> Result<()> {
        self.keys.write(tiles)?;
        self.apply(Event::KeysChanged, 0)
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
                Effect::Commit(update) => self.commit(update)?,
                Effect::Presented(token) => self.outcomes.push(Outcome::Presented { token, at_us: flipped_at_us }),
                Effect::Release(token) => {
                    let picture = self.pictures.remove(&token).context("released a picture that was never submitted")?;
                    if let Picture::Decoded(_) = picture {
                        let fb = self.framebuffers.remove(&token).context("released a picture with no framebuffer")?;
                        self.card.destroy_framebuffer(fb)?;
                    }
                }
                Effect::Drop(token) => {
                    self.pictures.remove(&token);
                    self.outcomes.push(Outcome::Dropped { token });
                }
                Effect::KeysFlipped => self.keys.front = 1 - self.keys.front,
            }
        }
        Ok(())
    }

    fn commit(&mut self, update: Update) -> Result<()> {
        let mut request = match update.video() {
            Some(token) => self.video_request(token)?,
            None => AtomicModeReq::new(),
        };
        if update.keys() {
            let fb = self.keys.prepare_back();
            request.add_property(self.keys.plane, self.keys.fb_id, Value::Framebuffer(Some(fb)));
        }
        self.card
            .atomic_commit(AtomicCommitFlags::NONBLOCK | AtomicCommitFlags::PAGE_FLIP_EVENT, request)
            .context("committing to the display")
    }

    fn video_request(&mut self, token: u64) -> Result<AtomicModeReq> {
        let picture = self.pictures.get(&token).context("committing a picture that was never submitted")?;
        let (fb, size, color) = match picture {
            Picture::Decoded(decoded) => {
                let fb = decoded_framebuffer(&self.card, decoded)?;
                self.framebuffers.insert(token, fb);
                (fb, decoded.size(), Some(decoded.color()))
            }
            Picture::Shared(shared) => (shared.buffer.fb, shared.buffer.size, None),
        };
        let mut request = self.video.show(fb, self.crtc, size, self.size);
        if let Some(color) = color.filter(|&color| self.applied_color != Some(color)) {
            let video = &self.video;
            let (encoding, range) = color_names(color);
            let encoding = enum_value(&self.card, video.color_encoding, encoding)?;
            let range = enum_value(&self.card, video.color_range, range)?;
            request.add_property(video.handle, video.color_encoding, Value::Unknown(encoding));
            request.add_property(video.handle, video.color_range, Value::Unknown(range));
            println!("Video color: {color:?}");
            self.applied_color = Some(color);
        }
        Ok(request)
    }

    /// Lets a renderer on the keyboard hand over scanout buffers from another thread.
    pub fn registrar(&self) -> Registrar {
        Registrar { card: Arc::clone(&self.card), crtc: self.crtc, video: self.video.clone(), screen: self.size, rotation: self.rotation }
    }

    /// What rings to wake `wait`: a new picture or new tiles.
    pub fn doorbell(&self) -> Arc<Doorbell> {
        Arc::clone(&self.doorbell)
    }

    /// Sleeps until a page flip finishes, the doorbell rings, or `timeout` passes.
    pub fn wait(&mut self, timeout: Duration) -> Result<()> {
        let timeout = Timespec::try_from(timeout).context("the compose loop's wait is too long")?;
        let mut fds = [PollFd::new(&*self.card, PollFlags::IN), PollFd::new(&*self.doorbell, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(e).context("waiting for the display"),
        }
        self.doorbell.answer();
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        true
    }

    pub fn set_status(&mut self, _status: &str) {}
}

fn decoded_framebuffer(card: &Card, picture: &super::mpp::Picture) -> Result<framebuffer::Handle> {
    let (width, height) = picture.size();
    let (hor_stride, ver_stride) = picture.strides();
    let handle = card.prime_fd_to_buffer(picture.dma_buf()).context("importing the decoded picture")?;
    let nv12 = Nv12 { size: (width, height), handle, pitch: hor_stride, chroma_offset: hor_stride * ver_stride };
    let fb = card.add_planar_framebuffer(&nv12, FbCmd2Flags::empty());
    card.close_buffer(handle)?;
    fb.context("wrapping the decoded picture in a framebuffer")
}

/// A renderer's buffer, wrapped in a framebuffer once and shown as often as it is presented.
pub struct SharedBuffer {
    card: Arc<Card>,
    pub fb: framebuffer::Handle,
    pub size: (u32, u32),
}

impl Drop for SharedBuffer {
    fn drop(&mut self) {
        if let Err(e) = self.card.destroy_framebuffer(self.fb) {
            eprintln!("error: freeing a shared framebuffer: {e}");
        }
    }
}

#[derive(Clone)]
pub struct Registrar {
    card: Arc<Card>,
    crtc: crtc::Handle,
    video: VideoPlane,
    screen: (u32, u32),
    rotation: PanelRotation,
}

impl Registrar {
    /// How renderers must draw: the video plane can't turn on this display controller.
    pub fn rotation(&self) -> PanelRotation {
        self.rotation
    }

    /// Imports the buffer and checks with a test-only commit that the video plane can show it.
    pub fn register(&self, dma_buf: &OwnedFd, layout: BufferLayout) -> Result<SharedBuffer> {
        let format = match DrmFourcc::try_from(layout.fourcc) {
            Ok(format @ (DrmFourcc::Xrgb8888 | DrmFourcc::Argb8888)) => format,
            _ => bail!("buffer format {:#x} is not XRGB8888 or ARGB8888", layout.fourcc),
        };
        let handle = self.card.prime_fd_to_buffer(dma_buf.as_fd()).context("importing the buffer")?;
        let single = SinglePlane { layout, format, handle };
        let flags = if layout.modifier == LINEAR { FbCmd2Flags::empty() } else { FbCmd2Flags::MODIFIERS };
        let fb = self.card.add_planar_framebuffer(&single, flags);
        self.card.close_buffer(handle)?;
        let buffer = SharedBuffer {
            card: Arc::clone(&self.card),
            fb: fb.context("wrapping the buffer in a framebuffer")?,
            size: (layout.width, layout.height),
        };
        let request = self.video.show(buffer.fb, self.crtc, buffer.size, self.screen);
        self.card
            .atomic_commit(AtomicCommitFlags::TEST_ONLY, request)
            .context("the video plane cannot show this buffer")?;
        Ok(buffer)
    }
}

const LINEAR: u64 = 0;

struct SinglePlane {
    layout: BufferLayout,
    format: DrmFourcc,
    handle: BufferHandle,
}

impl PlanarBuffer for SinglePlane {
    fn size(&self) -> (u32, u32) {
        (self.layout.width, self.layout.height)
    }

    fn format(&self) -> DrmFourcc {
        self.format
    }

    fn modifier(&self) -> Option<DrmModifier> {
        (self.layout.modifier != LINEAR).then(|| DrmModifier::from(self.layout.modifier))
    }

    fn pitches(&self) -> [u32; 4] {
        [self.layout.stride, 0, 0, 0]
    }

    fn handles(&self) -> [Option<BufferHandle>; 4] {
        [Some(self.handle), None, None, None]
    }

    fn offsets(&self) -> [u32; 4] {
        [self.layout.offset, 0, 0, 0]
    }
}

impl Surface {
    fn new(card: &Card, size: (u32, u32), premultiplied_argb: &[u32]) -> Result<Self> {
        let buffer = card.create_dumb_buffer(size, DrmFourcc::Argb8888, 32).context("allocating the label buffer")?;
        ensure!(buffer.pitch() % 4 == 0, "the label buffer's rows aren't whole pixels");
        let stride = buffer.pitch() as usize / 4;
        let mut pixels = Mapped::new(card, &buffer)?;
        let words = pixels.words();
        for (row, source) in premultiplied_argb.chunks_exact(size.0 as usize).enumerate() {
            words[row * stride..][..source.len()].copy_from_slice(source);
        }
        let fb = card.add_framebuffer(&buffer, 32, 32).context("wrapping the label buffer in a framebuffer")?;
        Ok(Self { _buffer: buffer, fb, pixels, stride })
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
    /// The request that puts `fb` on this plane, scaled to fit the screen.
    fn show(&self, fb: framebuffer::Handle, crtc: crtc::Handle, size: (u32, u32), screen: (u32, u32)) -> AtomicModeReq {
        let mut request = AtomicModeReq::new();
        request.add_property(self.handle, self.fb_id, Value::Framebuffer(Some(fb)));
        request.add_property(self.handle, self.crtc_id, Value::CRTC(Some(crtc)));
        let (sx, sy, sw, sh) = whole(size);
        for (property, value) in self.src.iter().zip([sx, sy, sw, sh]) {
            request.add_property(self.handle, *property, Value::UnsignedRange(value as u64));
        }
        let (dx, dy, dw, dh) = fit(size, screen);
        let dst = [
            Value::SignedRange(dx as i64),
            Value::SignedRange(dy as i64),
            Value::UnsignedRange(dw as u64),
            Value::UnsignedRange(dh as u64),
        ];
        for (property, value) in self.dst.iter().zip(dst) {
            request.add_property(self.handle, *property, value);
        }
        request
    }

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
