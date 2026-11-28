//! Where glassdeckd keeps things, and `config.json`: which keyboard to talk to and how fast the Link is.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::link::USB2_PRACTICAL_BYTES_PER_SEC;

pub const CONFIG_FORMAT_VERSION: u32 = 1;

/// Where the keyboard is: `usb`, through the receiver's own USB function, or a network address (an
/// Orange Pi standing in for it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum KeyboardAddress {
    Usb,
    Network(SocketAddr),
}

impl FromStr for KeyboardAddress {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == "usb" {
            return Ok(Self::Usb);
        }
        text.parse().map(Self::Network).map_err(|_| format!("{text:?} is neither \"usb\" nor an address like 10.0.0.186:7878"))
    }
}

impl TryFrom<String> for KeyboardAddress {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl fmt::Display for KeyboardAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usb => f.write_str("usb"),
            Self::Network(address) => address.fmt(f),
        }
    }
}

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
    keyboard: KeyboardAddress,
    /// `null` sends as fast as the network allows; required, so leaving it out is a mistake, not a choice.
    #[serde(deserialize_with = "Option::deserialize")]
    link_mbytes_per_sec: Option<u64>,
}

pub struct Config {
    pub keyboard: KeyboardAddress,
    pub link_bytes_per_sec: Option<u64>,
}

/// `--keyboard` wins over the file; with no file, a network Link is throttled like USB 2.0 and a USB
/// one is left to USB.
pub fn load(paths: &Paths, keyboard: Option<KeyboardAddress>) -> Result<Config> {
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
            let link_bytes_per_sec = match keyboard {
                KeyboardAddress::Usb => None,
                KeyboardAddress::Network(_) => Some(USB2_PRACTICAL_BYTES_PER_SEC),
            };
            return Ok(Config { keyboard, link_bytes_per_sec });
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

    #[test]
    fn the_keyboard_is_usb_or_an_address() {
        let usb = r#"{ "format": 1, "keyboard": "usb", "link_mbytes_per_sec": null }"#;
        assert_eq!(serde_json::from_str::<RawConfig>(usb).unwrap().keyboard, KeyboardAddress::Usb);
        let network: KeyboardAddress = "10.0.0.186:7878".parse().unwrap();
        assert_eq!(network, KeyboardAddress::Network("10.0.0.186:7878".parse().unwrap()));
        assert_eq!(network.to_string(), "10.0.0.186:7878");
        assert_eq!(KeyboardAddress::Usb.to_string(), "usb");
        assert!("USB".parse::<KeyboardAddress>().is_err());
        assert!(serde_json::from_str::<RawConfig>(r#"{ "format": 1, "keyboard": "keyboard", "link_mbytes_per_sec": null }"#).is_err());
    }
}
