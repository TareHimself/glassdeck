//! glassdeck-gpu: the keyboard's own GPU renderer. It draws the built-in canvas, shown whenever
//! nothing else owns the screen, or a canvas background, into buffers it hands the receiver's compositor.

#[cfg(target_os = "linux")]
mod compositor;
#[cfg(target_os = "linux")]
mod gpu;

use clap::Parser;

#[derive(Parser)]
#[command(about = "The keyboard's GPU renderer: draws the built-in canvas or a canvas background for the receiver")]
struct Cli {
    /// The receiver's compositor socket, which it passes when it starts us
    #[arg(long)]
    compositor_fd: Option<i32>,
    /// Draw this canvas background (.deckbg) instead of the built-in gradient
    #[arg(long)]
    canvas: Option<std::path::PathBuf>,
    /// Draw this many frames with no receiver, then print what they cost
    #[arg(long)]
    bench: Option<u32>,
    /// Draw one frame at 10 s and write it here as raw RGBA, to compare canvases
    #[arg(long)]
    snapshot: Option<std::path::PathBuf>,
}

/// On the PC only --snapshot works, through the same shader and script the keyboard runs.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use glassdeck_package::{Background, CanvasFiles};
    use glassdeck_gpu::offscreen::Offscreen;
    use glassdeck_gpu::program::CanvasProgram;

    let cli = Cli::parse();
    let path = cli.snapshot.context("on the PC, glassdeck-gpu only takes --snapshot")?;
    let files = match &cli.canvas {
        Some(canvas) => CanvasFiles::read(&Background::open(canvas)?)?,
        None => CanvasFiles::from_bytes(glassdeck_gpu::BUILT_IN)?,
    };
    let size = glassdeck_gpu::program::render_size(files.plan.scale);
    let offscreen = Offscreen::new(size)?;
    println!("glassdeck-gpu: {} at {}x{}", offscreen.renderer_name(), size.0, size.1);
    let mut canvas = CanvasProgram::new(&offscreen.gl, files)?;
    canvas.draw(&offscreen.gl, offscreen.framebuffer(), size, 10.0)?;
    // Bottom row first, as the keyboard's snapshots come out of glReadPixels, so the two compare directly.
    let pixels = offscreen.read_rgb();
    let rgba: Vec<u8> =
        pixels.chunks_exact(size.0 as usize).rev().flatten().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
    std::fs::write(&path, rgba).with_context(|| format!("writing {}", path.display()))
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::run(Cli::parse())
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::fd::AsFd;
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};
    use glassdeck_package::{Background, CanvasFiles};
    use glassdeck_proto::compositor::FromCompositor;

    use super::Cli;
    use crate::compositor::{Compositor, Received};
    use crate::gpu::{Gpu, Target};
    use glassdeck_gpu::BUILT_IN;
    use glassdeck_gpu::program::CanvasProgram as Canvas;

    /// One on screen, one waiting for the flip, one being drawn.
    const BUFFERS: usize = 3;
    const STATS_EVERY: Duration = Duration::from_secs(5);

    pub fn run(cli: Cli) -> Result<()> {
        let gpu = Gpu::open()?;
        let files = match &cli.canvas {
            Some(path) => CanvasFiles::read(&Background::open(path)?).with_context(|| format!("opening the canvas {}", path.display()))?,
            None => CanvasFiles::from_bytes(BUILT_IN).context("opening the built-in gradient")?,
        };
        let mut canvas = Canvas::new(&gpu.gl, files)?;
        let (scale, fps) = (canvas.plan.scale, canvas.plan.frame_rate);
        let size = glassdeck_gpu::program::render_size(scale);
        let targets = (0..BUFFERS).map(|_| gpu.target(size.0, size.1)).collect::<Result<Vec<_>>>()?;
        println!("glassdeck-gpu: {} at {}x{}, modifier {:#x}", gpu.renderer_name(), size.0, size.1, targets[0].layout.modifier);
        if let Some(path) = &cli.snapshot {
            return snapshot(&gpu, &targets[0], &mut canvas, path);
        }
        match (cli.compositor_fd, cli.bench) {
            (Some(fd), None) => {
                // SAFETY: the receiver hands us this socket at this number and nothing else owns it.
                let compositor = unsafe { Compositor::from_fd(fd) };
                present(&gpu, &targets, &mut canvas, &compositor, fps)
            }
            (None, Some(frames)) => bench(&gpu, &targets, &mut canvas, frames),
            _ => bail!("give either --compositor-fd (from the receiver) or --bench"),
        }
    }

    fn size_of(target: &Target) -> (u32, u32) {
        (target.layout.width, target.layout.height)
    }

    fn present(gpu: &Gpu, targets: &[Target], canvas: &mut Canvas, compositor: &Compositor, fps: u32) -> Result<()> {
        let ids = targets.iter().map(|t| compositor.register(t.dma_buf.as_fd(), t.layout)).collect::<Result<Vec<_>>>()?;
        let mut free: Vec<usize> = (0..targets.len()).collect();
        let period = Duration::from_secs_f64(1.0 / f64::from(fps));
        let started = Instant::now();
        let mut due = started;
        let mut stats = Stats::new();
        loop {
            let wait = free.is_empty();
            match compositor.receive(wait)? {
                Received::Closed => return Ok(()),
                Received::Message(FromCompositor::Released(id)) => {
                    let index = ids.iter().position(|&known| known == id).context("released a buffer we never registered")?;
                    free.push(index);
                    continue;
                }
                Received::Message(FromCompositor::Data(payload)) => {
                    canvas.deliver(&payload)?;
                    continue;
                }
                Received::Message(other) => bail!("the receiver sent {other:?} outside registration"),
                Received::Nothing => {}
            }
            let now = Instant::now();
            if due > now {
                thread::sleep(due - now);
            }
            due = (due + period).max(Instant::now());
            let index = free.pop().expect("a free buffer, waited for above");
            match canvas.draw(&gpu.gl, targets[index].framebuffer, size_of(&targets[index]), started.elapsed().as_secs_f32())? {
                Some(took) => {
                    stats.add(took, targets[index].layout.width, targets[index].layout.height);
                    compositor.present(ids[index])?;
                }
                None => free.push(index),
            }
        }
    }

    fn bench(gpu: &Gpu, targets: &[Target], canvas: &mut Canvas, frames: u32) -> Result<()> {
        let mut stats = Stats::new();
        let started = Instant::now();
        for frame in 0..frames {
            let target = &targets[frame as usize % targets.len()];
            if let Some(took) = canvas.draw(&gpu.gl, target.framebuffer, size_of(target), started.elapsed().as_secs_f32())? {
                stats.add(took, target.layout.width, target.layout.height);
            }
        }
        stats.print();
        Ok(())
    }

    fn snapshot(gpu: &Gpu, target: &Target, canvas: &mut Canvas, path: &std::path::Path) -> Result<()> {
        use glow::HasContext;
        canvas.draw(&gpu.gl, target.framebuffer, size_of(target), 10.0)?;
        let (w, h) = (target.layout.width, target.layout.height);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        // SAFETY: the context is current and the target was just drawn.
        unsafe {
            gpu.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.framebuffer));
            let into = glow::PixelPackData::Slice(Some(&mut pixels));
            gpu.gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, into);
        }
        std::fs::write(path, pixels).with_context(|| format!("writing {}", path.display()))
    }

    struct Stats {
        since: Instant,
        frames: u32,
        total: Duration,
        worst: Duration,
        size: (u32, u32),
    }

    impl Stats {
        fn new() -> Self {
            Self { since: Instant::now(), frames: 0, total: Duration::ZERO, worst: Duration::ZERO, size: (0, 0) }
        }

        /// Prints and starts over every `STATS_EVERY`.
        fn add(&mut self, took: Duration, width: u32, height: u32) {
            self.frames += 1;
            self.total += took;
            self.worst = self.worst.max(took);
            self.size = (width, height);
            if self.since.elapsed() >= STATS_EVERY {
                self.print();
                *self = Self::new();
            }
        }

        fn print(&self) {
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            println!(
                "canvas: {:.1} fps, draw {:.2} ms avg, {:.2} max at {}x{}",
                f64::from(self.frames) / self.since.elapsed().as_secs_f64(),
                ms(self.total) / f64::from(self.frames.max(1)),
                ms(self.worst),
                self.size.0,
                self.size.1
            );
        }
    }
}
