//! Mutes one process in the Windows volume mixer, and puts every session back as it was afterwards.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context as _, Result};
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
    MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::Interface;

/// A process can open new audio sessions at any time (a scene starts its sound after loading), so keep looking.
const RESCAN: Duration = Duration::from_millis(250);

pub struct ProcessMuted {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl ProcessMuted {
    pub fn start(pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let watching = Arc::clone(&stop);
        let thread = thread::spawn(move || keep_muted(pid, &watching));
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for ProcessMuted {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(Ok(()))) | None => {}
            Some(Ok(Err(e))) => eprintln!("error: muting Wallpaper Engine: {e:#}"),
            Some(Err(_)) => eprintln!("error: the mute thread panicked"),
        }
    }
}

fn keep_muted(pid: u32, stop: &AtomicBool) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("starting COM")? };
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).context("opening the audio devices")? };
    let mut muted_by_us: Vec<ISimpleAudioVolume> = Vec::new();
    let result = (|| {
        while !stop.load(Ordering::Relaxed) {
            for volume in sessions_of(&enumerator, pid)? {
                if !unsafe { volume.GetMute()? }.as_bool() {
                    unsafe { volume.SetMute(true, std::ptr::null())? };
                    muted_by_us.push(volume);
                }
            }
            thread::sleep(RESCAN);
        }
        Ok(())
    })();
    for volume in muted_by_us {
        if let Err(e) = unsafe { volume.SetMute(false, std::ptr::null()) } {
            eprintln!("error: unmuting an audio session: {e}");
        }
    }
    result
}

fn sessions_of(enumerator: &IMMDeviceEnumerator, pid: u32) -> Result<Vec<ISimpleAudioVolume>> {
    let mut found = Vec::new();
    unsafe {
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for i in 0..devices.GetCount()? {
            let manager: IAudioSessionManager2 = devices.Item(i)?.Activate(CLSCTX_ALL, None)?;
            let sessions = manager.GetSessionEnumerator()?;
            for j in 0..sessions.GetCount()? {
                let control = sessions.GetSession(j)?;
                if control.cast::<IAudioSessionControl2>()?.GetProcessId()? == pid {
                    found.push(control.cast::<ISimpleAudioVolume>()?);
                }
            }
        }
    }
    Ok(found)
}
