//! glassdeck-scene: the keyboard's own GPU renderer. For now it draws the built-in scene, shown
//! whenever nothing else owns the screen, into buffers it hands the receiver's compositor.

#[cfg(target_os = "linux")]
mod compositor;
#[cfg(target_os = "linux")]
mod gpu;
#[cfg(target_os = "linux")]
mod gradient;

use clap::Parser;

#[derive(Parser)]
#[command(about = "The keyboard's GPU renderer: draws the built-in scene for the receiver")]
struct Cli {
    /// The receiver's compositor socket, which it passes when it starts us
    #[arg(long)]
    compositor_fd: Option<i32>,
    /// Render size as a fraction of the panel; the display scales it up for free
    #[arg(long, default_value_t = 0.5)]
    scale: f32,
    /// The built-in scene drifts slowly enough that 30 looks the same as 60, at half the GPU time
    #[arg(long, default_value_t = 30)]
    fps: u32,
    /// Draw this many frames with no receiver, then print what they cost
    #[arg(long)]
    bench: Option<u32>,
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    let _ = Cli::parse();
    anyhow::bail!("glassdeck-scene runs on the keyboard only")
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

    use anyhow::{Context, Result, bail, ensure};
    use glassdeck_proto::compositor::FromCompositor;

    use super::Cli;
    use crate::compositor::{Compositor, Received};
    use crate::gpu::{Gpu, Target};
    use crate::gradient::Gradient;

    const PANEL: (u32, u32) = (1920, 1080);
    /// One on screen, one waiting for the flip, one being drawn.
    const BUFFERS: usize = 3;
    const STATS_EVERY: Duration = Duration::from_secs(5);

    pub fn run(cli: Cli) -> Result<()> {
        ensure!(cli.scale > 0.0 && cli.scale <= 1.0, "--scale must be in (0, 1]");
        ensure!(cli.fps > 0, "--fps must be above 0");
        let gpu = Gpu::open()?;
        let size = scaled(cli.scale);
        let targets = (0..BUFFERS).map(|_| gpu.target(size.0, size.1)).collect::<Result<Vec<_>>>()?;
        let gradient = Gradient::new(&gpu.gl)?;
        println!(
            "glassdeck-scene: {} at {}x{}, modifier {:#x}",
            gpu.renderer_name(),
            size.0,
            size.1,
            targets[0].layout.modifier
        );
        match (cli.compositor_fd, cli.bench) {
            (Some(fd), None) => {
                // SAFETY: the receiver hands us this socket at this number and nothing else owns it.
                let compositor = unsafe { Compositor::from_fd(fd) };
                present(&gpu, &targets, &gradient, &compositor, cli.fps)
            }
            (None, Some(frames)) => {
                bench(&gpu, &targets, &gradient, frames);
                Ok(())
            }
            _ => bail!("give either --compositor-fd (from the receiver) or --bench"),
        }
    }

    /// Even sizes, which every scaler handles.
    fn scaled(scale: f32) -> (u32, u32) {
        let side = |panel: u32| (((panel as f32 * scale) / 2.0).round() as u32 * 2).max(2);
        (side(PANEL.0), side(PANEL.1))
    }

    fn present(gpu: &Gpu, targets: &[Target], gradient: &Gradient, compositor: &Compositor, fps: u32) -> Result<()> {
        let ids = targets
            .iter()
            .map(|t| compositor.register(t.dma_buf.as_fd(), t.layout))
            .collect::<Result<Vec<_>>>()?;
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
                Received::Message(other) => bail!("the receiver sent {other:?} outside registration"),
                Received::Nothing => {}
            }
            let now = Instant::now();
            if due > now {
                thread::sleep(due - now);
            }
            due = (due + period).max(Instant::now());
            let index = free.pop().expect("a free buffer, waited for above");
            let took = gradient.draw(&gpu.gl, &targets[index], started.elapsed().as_secs_f32());
            stats.add(took, targets[index].layout.width, targets[index].layout.height);
            compositor.present(ids[index])?;
        }
    }

    fn bench(gpu: &Gpu, targets: &[Target], gradient: &Gradient, frames: u32) {
        let mut stats = Stats::new();
        let started = Instant::now();
        for frame in 0..frames {
            let target = &targets[frame as usize % targets.len()];
            let took = gradient.draw(&gpu.gl, target, started.elapsed().as_secs_f32());
            stats.add(took, target.layout.width, target.layout.height);
        }
        stats.print();
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
                "scene: {:.1} fps, draw {:.2} ms avg, {:.2} max at {}x{}",
                f64::from(self.frames) / self.since.elapsed().as_secs_f64(),
                ms(self.total) / f64::from(self.frames.max(1)),
                ms(self.worst),
                self.size.0,
                self.size.1
            );
        }
    }
}
