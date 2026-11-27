//! The hosts a package's `host.luau` may reach with `net`, from `"network"` in `deck.json`.

use std::fmt;

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

/// One entry of `"network"`: a host name, or `*.` and a domain for its subdomains (not the domain itself).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum NetworkHost {
    Exact(String),
    Subdomains(String),
}

impl NetworkHost {
    pub fn parse(text: &str) -> Result<Self> {
        let (host, subdomains) = match text.strip_prefix("*.") {
            Some(domain) => (domain, true),
            None => (text, false),
        };
        ensure!(!host.is_empty(), "\"{text}\" names no host");
        if !host.split('.').all(|label| !label.is_empty() && label.len() <= 63 && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
        {
            bail!("\"{text}\" isn't a lower-case host name like api.example.com (or *.example.com for its subdomains)");
        }
        if subdomains && !host.contains('.') {
            bail!("\"{text}\" would match every subdomain of a top-level domain; name the domain, like *.example.com");
        }
        Ok(if subdomains { Self::Subdomains(host.to_owned()) } else { Self::Exact(host.to_owned()) })
    }

    /// Whether `host` (lower case, as a URL's host) is this entry.
    pub fn matches(&self, host: &str) -> bool {
        match self {
            Self::Exact(name) => host == name,
            Self::Subdomains(domain) => host.strip_suffix(domain.as_str()).is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
        }
    }
}

impl TryFrom<String> for NetworkHost {
    type Error = anyhow::Error;

    fn try_from(text: String) -> Result<Self> {
        Self::parse(&text)
    }
}

impl From<NetworkHost> for String {
    fn from(host: NetworkHost) -> Self {
        host.to_string()
    }
}

impl fmt::Display for NetworkHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(name) => write!(f, "{name}"),
            Self::Subdomains(domain) => write!(f, "*.{domain}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_matches_its_host_or_its_subdomains_only() {
        let exact = NetworkHost::parse("api.open-meteo.com").unwrap();
        assert!(exact.matches("api.open-meteo.com"));
        assert!(!exact.matches("evil.api.open-meteo.com"));
        let under = NetworkHost::parse("*.openweathermap.org").unwrap();
        assert!(under.matches("api.openweathermap.org"));
        assert!(under.matches("a.b.openweathermap.org"));
        assert!(!under.matches("openweathermap.org"));
        assert!(!under.matches("evilopenweathermap.org"));
        assert_eq!(under.to_string(), "*.openweathermap.org");
    }

    #[test]
    fn malformed_entries_are_refused() {
        for bad in ["", "*", "*.com", "API.example.com", "https://api.example.com", "api..example.com", "api.example.com/path"] {
            assert!(NetworkHost::parse(bad).is_err(), "{bad}");
        }
    }
}
