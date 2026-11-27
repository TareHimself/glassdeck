//! `audio`: what the host's speakers are playing, as loudness per frequency band. Capture starts the
//! first time a script asks and stops when the last host side using the tap is gone.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Samples the spectrum is taken over: about 43 ms at 48 kHz, enough to tell low bands apart.
const WINDOW: usize = 2048;
/// With nothing new for this long, nothing is playing (loopback sends no packets during silence).
const SILENT_AFTER: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_millis(10);
const LOWEST_HZ: f32 = 40.0;
const HIGHEST_HZ: f32 = 16_000.0;
/// Band loudness from this quiet (dB below a full-scale tone) reads as 0, up to full scale as 1.
const FLOOR_DB: f32 = -60.0;
pub const MAX_BANDS: usize = 64;

pub type SharedAudio = Arc<AudioTap>;

pub fn audio_tap() -> SharedAudio {
    Arc::new(AudioTap { capture: Arc::new(Mutex::new(Capture::NotStarted)), fft: FftPlanner::new().plan_fft_forward(WINDOW) })
}

/// The latest samples from the default output device, mixed to mono.
pub struct AudioTap {
    capture: Arc<Mutex<Capture>>,
    fft: Arc<dyn Fft<f32>>,
}

enum Capture {
    NotStarted,
    Running { samples: VecDeque<f32>, rate: u32, last_packet: Instant },
    Failed(String),
}

impl AudioTap {
    /// Loudness from 0 to 1 in `count` bands, spaced evenly in pitch from 40 Hz to 16 kHz.
    pub fn bands(&self, count: usize) -> Result<Vec<f32>> {
        ensure!((1..=MAX_BANDS).contains(&count), "audio.bands takes 1 to {MAX_BANDS} bands, not {count}");
        let (window, rate) = {
            let mut capture = self.capture.lock().expect("audio capture poisoned");
            match &*capture {
                Capture::NotStarted => {
                    *capture = Capture::Running { samples: VecDeque::with_capacity(WINDOW * 2), rate: 48_000, last_packet: Instant::now() };
                    start(Arc::downgrade(&self.capture))?;
                    return Ok(vec![0.0; count]);
                }
                Capture::Failed(why) => bail!("can't hear the speakers: {why}"),
                Capture::Running { samples, rate, last_packet } => {
                    if last_packet.elapsed() > SILENT_AFTER || samples.len() < WINDOW {
                        return Ok(vec![0.0; count]);
                    }
                    (samples.iter().skip(samples.len() - WINDOW).copied().collect::<Vec<_>>(), *rate)
                }
            }
        };
        Ok(spectrum_bands(self.fft.as_ref(), &window, rate, count))
    }
}

/// Hann-windowed spectrum of `window`, averaged into `count` log-spaced bands and scaled to 0..1.
fn spectrum_bands(fft: &dyn Fft<f32>, window: &[f32], rate: u32, count: usize) -> Vec<f32> {
    let n = window.len();
    let mut buffer: Vec<Complex<f32>> = window
        .iter()
        .enumerate()
        .map(|(i, s)| Complex::new(s * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()), 0.0))
        .collect();
    fft.process(&mut buffer);
    let bin_hz = rate as f32 / n as f32;
    // A full-scale tone through a Hann window peaks at n / 4.
    let full_scale = (n as f32 / 4.0).powi(2);
    let highest = HIGHEST_HZ.min(rate as f32 / 2.0);
    let edge = |i: usize| LOWEST_HZ * (highest / LOWEST_HZ).powf(i as f32 / count as f32);
    (0..count)
        .map(|band| {
            let (from, to) = (edge(band) / bin_hz, edge(band + 1) / bin_hz);
            let first = (from.floor() as usize).max(1);
            let last = (to.ceil() as usize).clamp(first + 1, n / 2);
            let power = buffer[first..last].iter().map(|c| c.norm_sqr()).fold(0.0f32, f32::max);
            let db = 10.0 * (power / full_scale).max(1e-12).log10();
            ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
        })
        .collect()
}

fn start(capture: Weak<Mutex<Capture>>) -> Result<()> {
    thread::Builder::new()
        .name("audio tap".into())
        .spawn(move || {
            if let Err(e) = listen(&capture)
                && let Some(capture) = capture.upgrade()
            {
                *capture.lock().expect("audio capture poisoned") = Capture::Failed(format!("{e:#}"));
            }
        })
        .context("starting audio capture")?;
    Ok(())
}

/// Loopback capture of the default output device, until nobody holds the tap.
fn listen(capture: &Weak<Mutex<Capture>>) -> Result<()> {
    use windows::Win32::Media::Audio::{
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, eConsole, eRender,
    };
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};

    // SAFETY: plain COM calls on this thread, which initialises COM first; every pointer read comes
    // from WASAPI and is used only while it says it is valid.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("starting COM")?;
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).context("listing audio devices")?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole).context("finding the speakers")?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).context("opening the speakers")?;
        let format = client.GetMixFormat().context("reading the speakers' format")?;
        let (channels, rate, bits) = ((*format).nChannels as usize, (*format).nSamplesPerSec, (*format).wBitsPerSample);
        let initialized = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 200_000, 0, format, None);
        CoTaskMemFree(Some(format as *const _));
        initialized.context("listening to the speakers")?;
        if bits != 32 {
            return Err(anyhow!("the speakers mix {bits}-bit samples, not 32-bit float"));
        }
        let packets: IAudioCaptureClient = client.GetService().context("reading the speakers")?;
        client.Start().context("starting to listen")?;
        loop {
            thread::sleep(POLL);
            let Some(shared) = capture.upgrade() else { return Ok(()) };
            while packets.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                packets.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let mono: Vec<f32> = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    vec![0.0; frames as usize]
                } else {
                    let interleaved = std::slice::from_raw_parts(data as *const f32, frames as usize * channels);
                    interleaved.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32).collect()
                };
                packets.ReleaseBuffer(frames)?;
                if let Capture::Running { samples, rate: r, last_packet } = &mut *shared.lock().expect("audio capture poisoned") {
                    samples.extend(mono);
                    let extra = samples.len().saturating_sub(WINDOW * 2);
                    samples.drain(..extra);
                    *r = rate;
                    *last_packet = Instant::now();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amplitude: f32, rate: u32) -> Vec<f32> {
        (0..WINDOW).map(|i| amplitude * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin()).collect()
    }

    #[test]
    fn a_tone_lights_its_own_band_and_silence_none() {
        let fft = FftPlanner::new().plan_fft_forward(WINDOW);
        let bands = spectrum_bands(fft.as_ref(), &tone(1000.0, 0.5, 48_000), 48_000, 16);
        let loudest = bands.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let edge = |i: usize| LOWEST_HZ * (HIGHEST_HZ / LOWEST_HZ).powf(i as f32 / 16.0);
        assert!(edge(loudest) <= 1000.0 && 1000.0 < edge(loudest + 1), "1 kHz landed in band {loudest}: {bands:?}");
        assert!(bands[loudest] > 0.8, "{bands:?}");
        assert!(bands[0] < 0.3 && bands[15] < 0.3, "{bands:?}");
        let quiet = spectrum_bands(fft.as_ref(), &vec![0.0; WINDOW], 48_000, 16);
        assert!(quiet.iter().all(|b| *b == 0.0));
    }
}
