use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use glassdeck_deck::{Deck, Entry};
use glassdeck_proto::unix_micros;
use windows::Win32::Foundation::{HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Query,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::core::{BOOL, Interface};

use crate::gpu::GpuEncoder;
use crate::pipeline::{EncodedFrame, EncoderSettings, FrameSource, Timing, VideoFormat, request_stop, stop_requested};

const POLL: Duration = Duration::from_millis(50);
/// Pages inside a deck load from here; the shim serves each path from the deck's entry of that name.
pub const DECK_ORIGIN: &str = "deck://wallpaper/";

type FrameFn = extern "C" fn(user: *mut c_void, shared_handle: *mut c_void, width: u32, height: u32, timestamp_us: u64);

#[repr(C)]
struct DeckProvider {
    user: *mut c_void,
    lookup: extern "C" fn(*mut c_void, *const c_char, *mut u64, *mut u64, *mut c_char, u32) -> i32,
    read: extern "C" fn(*mut c_void, u64, u64, u64, *mut c_void, u32) -> i64,
}

unsafe extern "C" {
    fn gd_web_subprocess() -> i32;
    fn gd_web_start(
        url: *const c_char,
        init_script: *const c_char,
        deck: *const DeckProvider,
        width: u32,
        height: u32,
        fps: u32,
        cache_dir: *const c_char,
        on_frame: FrameFn,
        user: *mut c_void,
    ) -> i32;
    fn gd_web_stop();
}

/// Chromium relaunches our executable for its renderer and GPU processes, marked by a `--type=` switch.
pub fn helper_process_exit_code() -> Option<i32> {
    std::env::args().any(|a| a.starts_with("--type=")).then(|| unsafe { gd_web_subprocess() })
}

/// A web page as a raw-frame source: CEF renders offscreen into shared GPU textures, which go to the encoder.
/// Wallpapers are paced.
pub struct WebSource {
    url: CString,
    init_script: CString,
    deck: Option<Arc<Deck>>,
    format: VideoFormat,
    settings: EncoderSettings,
    duration: Option<Duration>,
}

impl WebSource {
    pub fn new(url: &str, settings: EncoderSettings, duration: Option<Duration>) -> Result<Self> {
        Ok(Self {
            url: CString::new(url).context("URL contains a NUL byte")?,
            init_script: CString::default(),
            deck: None,
            format: VideoFormat {
                codec: settings.codec,
                width: u16::try_from(settings.width).context("page too wide")?,
                height: u16::try_from(settings.height).context("page too tall")?,
            },
            settings,
            duration,
        })
    }

    /// Runs `script` in the page before the page's own scripts, as Wallpaper Engine's hooks need.
    pub fn with_init_script(self, script: &str) -> Result<Self> {
        Ok(Self { init_script: CString::new(script).context("init script contains a NUL byte")?, ..self })
    }

    /// Serves `deck://wallpaper/<entry>` from the deck while the page runs.
    pub fn with_deck(self, deck: Arc<Deck>) -> Self {
        Self { deck: Some(deck), ..self }
    }
}

impl FrameSource for WebSource {
    fn format(&self) -> VideoFormat {
        self.format
    }

    fn timing(&self) -> Timing {
        Timing::Paced
    }

    fn run(self: Box<Self>, out: SyncSender<EncodedFrame>) -> Result<()> {
        let (device, context) = create_device()?;
        let encoder = GpuEncoder::start(&device, &context, self.settings, out)?;
        println!("Encoder: {}", encoder.name);
        let (width, height) = (self.settings.width, self.settings.height);
        let sink = Box::new(Mutex::new(FrameSink::new(&device, context, encoder, (width, height))?));

        let cache = CString::new(std::env::temp_dir().join("glassdeck-cef").to_string_lossy().as_bytes())?;
        let user = (&*sink as *const Mutex<FrameSink>).cast_mut().cast();
        let provider = self.deck.as_ref().map(|deck| DeckProvider {
            user: Arc::as_ptr(deck).cast_mut().cast(),
            lookup: deck_lookup,
            read: deck_read,
        });
        let provider_ptr = provider.as_ref().map_or(std::ptr::null(), |p| p as *const DeckProvider);
        let started = unsafe {
            gd_web_start(
                self.url.as_ptr(),
                self.init_script.as_ptr(),
                provider_ptr,
                width,
                height,
                self.settings.fps,
                cache.as_ptr(),
                on_frame,
                user,
            )
        };
        ensure!(started == 0, "CEF failed to start (step {started})");
        // Chromium adds a console handler that quits at once; Windows asks the newest handler first.
        unsafe { SetConsoleCtrlHandler(Some(on_console_signal), true) }.context("installing the Ctrl+C handler")?;
        println!("Rendering {} at {width}x{height}@{}", self.url.to_string_lossy(), self.settings.fps);

        let begun = Instant::now();
        let failure = loop {
            thread::sleep(POLL);
            if let Some(e) = sink.lock().map_err(|_| anyhow!("frame sink poisoned"))?.failure.take() {
                break Some(e);
            }
            if stop_requested() || self.duration.is_some_and(|d| begun.elapsed() >= d) {
                break None;
            }
        };
        unsafe { gd_web_stop() };
        let sink = sink.lock().map_err(|_| anyhow!("frame sink poisoned"))?;
        println!(
            "Rendered {} frames; each held CEF's thread {:.2} ms on average (max {:.2}) while we copied it",
            sink.frames,
            sink.callback_total.as_secs_f64() * 1000.0 / sink.frames.max(1) as f64,
            sink.callback_max.as_secs_f64() * 1000.0
        );
        failure.map_or(Ok(()), Err)
    }
}

/// Receives CEF's frames on its UI thread. The shared texture is only valid during the callback,
/// so each frame is copied into a texture we own and the GPU is drained before returning.
struct FrameSink {
    device: ID3D11Device1,
    context: ID3D11DeviceContext,
    encoder: GpuEncoder,
    size: (u32, u32),
    owned: Option<ID3D11Texture2D>,
    done: ID3D11Query,
    frames: u64,
    /// Time spent inside CEF's paint callback, which blocks its UI thread
    callback_total: Duration,
    callback_max: Duration,
    failure: Option<anyhow::Error>,
}

// SAFETY: D3D11 is free-threaded with multithread protection on (GpuEncoder::start turns it on),
// and the Mutex around the sink serialises callbacks.
unsafe impl Send for FrameSink {}

impl FrameSink {
    fn new(device: &ID3D11Device, context: ID3D11DeviceContext, encoder: GpuEncoder, size: (u32, u32)) -> Result<Self> {
        let mut done = None;
        unsafe { device.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut done))? };
        Ok(Self {
            device: device.cast().context("GPU lacks ID3D11Device1, needed to open CEF's shared textures")?,
            context,
            encoder,
            size,
            owned: None,
            done: done.context("CreateQuery returned nothing")?,
            frames: 0,
            callback_total: Duration::ZERO,
            callback_max: Duration::ZERO,
            failure: None,
        })
    }

    fn accept(&mut self, shared_handle: *mut c_void, width: u32, height: u32) -> Result<()> {
        let entered = Instant::now();
        if (width, height) != self.size {
            bail!("CEF painted {width}x{height}, expected {}x{}", self.size.0, self.size.1);
        }
        unsafe {
            let shared: ID3D11Texture2D = self
                .device
                .OpenSharedResource1(HANDLE(shared_handle))
                .context("opening CEF's shared texture")?;
            let owned = match &self.owned {
                Some(texture) => texture.clone(),
                None => self.owned.insert(self.create_owned(&shared)?).clone(),
            };
            let visible = D3D11_BOX { left: 0, top: 0, front: 0, right: width, bottom: height, back: 1 };
            self.context.CopySubresourceRegion(&owned, 0, 0, 0, 0, &shared, 0, Some(&visible));
            self.encoder.submit(&owned, unix_micros())?;
            self.wait_for_gpu()?;
        }
        let held = entered.elapsed();
        self.callback_total += held;
        self.callback_max = self.callback_max.max(held);
        self.frames += 1;
        Ok(())
    }

    /// CEF may reuse the shared texture once we return, so our reads of it must have finished.
    unsafe fn wait_for_gpu(&self) -> Result<()> {
        unsafe {
            self.context.End(&self.done);
            loop {
                // An event query writes a Win32 BOOL, and only once the GPU has reached it.
                let mut finished: i32 = 0;
                self.context
                    .GetData(&self.done, Some((&mut finished as *mut i32).cast()), size_of::<i32>() as u32, 0)
                    .context("waiting for the GPU")?;
                if finished != 0 {
                    return Ok(());
                }
                thread::yield_now();
            }
        }
    }

    unsafe fn create_owned(&self, like: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        unsafe {
            let mut source = D3D11_TEXTURE2D_DESC::default();
            like.GetDesc(&mut source);
            let desc = D3D11_TEXTURE2D_DESC {
                Width: self.size.0,
                Height: self.size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: source.Format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut texture = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut texture))?;
            println!("CEF frames are {:?}", source.Format);
            texture.context("CreateTexture2D returned nothing")
        }
    }
}

extern "C" fn deck_lookup(user: *mut c_void, path: *const c_char, offset: *mut u64, len: *mut u64, mime: *mut c_char, mime_cap: u32) -> i32 {
    let deck = unsafe { &*(user as *const Deck) };
    let Ok(path) = unsafe { CStr::from_ptr(path) }.to_str() else { return 1 };
    let Some(entry) = deck.entry(path) else {
        eprintln!("deck: the page asked for {path:?}, which the deck does not hold");
        return 1;
    };
    let kind = mime_type(path).as_bytes();
    let n = kind.len().min(mime_cap.saturating_sub(1) as usize);
    unsafe {
        *offset = entry.offset;
        *len = entry.len;
        std::ptr::copy_nonoverlapping(kind.as_ptr(), mime.cast::<u8>(), n);
        *mime.add(n) = 0;
    }
    0
}

extern "C" fn deck_read(user: *mut c_void, offset: u64, len: u64, at: u64, buf: *mut c_void, buf_len: u32) -> i64 {
    let deck = unsafe { &*(user as *const Deck) };
    let buf = unsafe { std::slice::from_raw_parts_mut(buf.cast::<u8>(), buf_len as usize) };
    match deck.read_range(Entry { offset, len }, at, buf) {
        Ok(n) => n as i64,
        Err(e) => {
            eprintln!("deck: {e:#}");
            -1
        }
    }
}

/// Content types by extension, for what web wallpapers load.
fn mime_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

unsafe extern "system" fn on_console_signal(_signal: u32) -> BOOL {
    request_stop();
    true.into()
}

extern "C" fn on_frame(user: *mut c_void, shared_handle: *mut c_void, width: u32, height: u32, _timestamp_us: u64) {
    let sink = unsafe { &*(user as *const Mutex<FrameSink>) };
    let Ok(mut sink) = sink.lock() else { return };
    if sink.failure.is_some() {
        return;
    }
    if let Err(e) = sink.accept(shared_handle, width, height) {
        sink.failure = Some(e);
    }
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .context("creating a D3D11 device")?;
    }
    Ok((device.context("no device")?, context.context("no device context")?))
}
