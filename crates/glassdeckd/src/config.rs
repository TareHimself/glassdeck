//! Where glassdeckd keeps things, and `config.json`: which keyboard to talk to and how fast the Link is.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::link::USB2_PRACTICAL_BYTES_PER_SEC;

pub const CONFIG_FORMAT_VERSION: u32 = 1;

pub struct Paths {
    pub config: PathBuf,
    /// The active theme, as the app last pushed it
    pub theme: PathBuf,
    /// Copies of the active theme's packages; the app keeps the library
    pub active: PathBuf,
    pub api: PathBuf,
    pub log: PathBuf,
}

impl Paths {
    pub fn for_user() -> Result<Self> {
        let roaming = PathBuf::from(std::env::var_os("APPDATA").context("APPDATA isn't set")?).join("Glassdeck");
        let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA isn't set")?).join("Glassdeck");
        Ok(Self {
            config: roaming.join("config.json"),
            theme: roaming.join("theme.json"),
            active: glassdeck_api::active_packages()?,
            api: glassdeck_api::api_file()?,
            log: local.join("logs").join("glassdeckd.log"),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    format: u32,
    keyboard: SocketAddr,
    /// `null` sends as fast as the network allows; required, so leaving it out is a mistake, not a choice.
    #[serde(deserialize_with = "Option::deserialize")]
    link_mbytes_per_sec: Option<u64>,
}

pub struct Config {
    pub keyboard: SocketAddr,
    pub link_bytes_per_sec: Option<u64>,
}

/// `--keyboard` wins over the file; with no file, the Link is throttled like USB 2.0.
pub fn load(paths: &Paths, keyboard: Option<SocketAddr>) -> Result<Config> {
    let text = match std::fs::read_to_string(&paths.config) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let Some(keyboard) = keyboard else {
                bail!(
                    "no keyboard address: pass --keyboard, or create {} like\n{}",
                    paths.config.display(),
                    r#"{ "format": 1, "keyboard": "10.0.0.186:7878", "link_mbytes_per_sec": 35 }"#
                );
            };
            return Ok(Config { keyboard, link_bytes_per_sec: Some(USB2_PRACTICAL_BYTES_PER_SEC) });
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", paths.config.display())),
    };
    let raw: RawConfig = serde_json::from_str(&text).with_context(|| format!("parsing {}", paths.config.display()))?;
    ensure!(raw.format == CONFIG_FORMAT_VERSION, "config format {} is not the {CONFIG_FORMAT_VERSION} this build reads", raw.format);
    Ok(Config { keyboard: keyboard.unwrap_or(raw.keyboard), link_bytes_per_sec: raw.link_mbytes_per_sec.map(|mb| mb * 1_000_000) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_speed_must_be_given_even_if_null() {
        let missing = r#"{ "format": 1, "keyboard": "10.0.0.186:7878" }"#;
        assert!(serde_json::from_str::<RawConfig>(missing).is_err());
        let unlimited = r#"{ "format": 1, "keyboard": "10.0.0.186:7878", "link_mbytes_per_sec": null }"#;
        assert_eq!(serde_json::from_str::<RawConfig>(unlimited).unwrap().link_mbytes_per_sec, None);
    }
}
