//! Windows plumbing: one glassdeckd per user, output to a log file, and starting at login.

use std::fs::{self, OpenOptions};
use std::os::windows::io::IntoRawHandle;
use std::path::Path;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Console::{FreeConsole, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::{HSTRING, w};

/// Past this, the log moves to `glassdeckd.log.old` when glassdeckd starts.
const MAX_LOG_BYTES: u64 = 10 << 20;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Glassdeck";

/// Held for glassdeckd's whole life; a second glassdeckd finds it and exits.
pub struct SingleInstance(#[allow(dead_code)] HANDLE);

pub fn single_instance(api_file: &Path) -> Result<SingleInstance> {
    let handle = unsafe { CreateMutexW(None, true, w!("Local\\glassdeckd")) }.context("creating the single-instance mutex")?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        bail!("glassdeckd is already running for this user (its API is in {})", api_file.display());
    }
    Ok(SingleInstance(handle))
}

/// Sends stdout and stderr (and so every source worker's output) to the log, and drops the console.
pub fn log_to(path: &Path) -> Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    }
    if fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        fs::rename(path, path.with_extension("log.old")).context("moving the old log aside")?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path).with_context(|| format!("opening {}", path.display()))?;
    let error = file.try_clone()?;
    unsafe {
        let _ = FreeConsole();
        SetStdHandle(STD_OUTPUT_HANDLE, HANDLE(file.into_raw_handle())).context("redirecting stdout to the log")?;
        SetStdHandle(STD_ERROR_HANDLE, HANDLE(error.into_raw_handle())).context("redirecting stderr to the log")?;
    }
    Ok(())
}

/// Adds `glassdeckd run --log` to this user's startup programs.
pub fn install() -> Result<()> {
    let exe = std::env::current_exe()?;
    let command = format!("\"{}\" run --log", exe.display());
    let value: Vec<u16> = command.encode_utf16().chain([0]).collect();
    let bytes = unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2) };
    with_run_key(|key| unsafe { RegSetValueExW(key, &HSTRING::from(RUN_VALUE), None, REG_SZ, Some(bytes)) }.ok())
        .context("adding glassdeckd to startup programs")?;
    println!("glassdeckd starts at login: {command}");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    with_run_key(|key| unsafe { RegDeleteValueW(key, &HSTRING::from(RUN_VALUE)) }.ok()).context("removing glassdeckd from startup programs")?;
    println!("glassdeckd no longer starts at login");
    Ok(())
}

fn with_run_key(act: impl FnOnce(HKEY) -> windows::core::Result<()>) -> Result<()> {
    let mut key = HKEY::default();
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), None, KEY_SET_VALUE, &mut key) }.ok()?;
    let result = act(key);
    let _ = unsafe { RegCloseKey(key) };
    Ok(result?)
}
