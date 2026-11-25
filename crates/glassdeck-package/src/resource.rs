//! Module resources: files a script asks for by kind and name, never by path. A name resolves only
//! inside its own module's `assets/<kind>/`, so a script can't reach anything else.

use std::fmt;

use anyhow::{Result, ensure};

use crate::module::ASSETS_FOLDER;

/// Which side a resource ships to; the kind picks its folder under `assets/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// Deployed with the module; usable on both sides.
    Keyboard,
    /// Stays on the host.
    Host,
}

impl ResourceKind {
    pub const ALL: [ResourceKind; 2] = [ResourceKind::Keyboard, ResourceKind::Host];

    /// The folder under `assets/`, which is also the Luau table that hands these resources out.
    pub const fn folder(self) -> &'static str {
        match self {
            Self::Keyboard => "keyboard",
            Self::Host => "host",
        }
    }
}

/// A checked resource name: `/`-separated segments of `a-z 0-9 . _ -`, none empty, `.` or `..`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceName(String);

impl ResourceName {
    pub fn parse(name: &str) -> Result<Self> {
        let plain_segment = |s: &str| {
            !s.is_empty() && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        };
        ensure!(
            name.split('/').all(plain_segment),
            "{name:?} isn't a resource name: use lowercase letters, digits, '.', '_' and '-', in folders split by '/'"
        );
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A file in a module, as a script names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Resource {
    pub kind: ResourceKind,
    pub name: ResourceName,
}

impl Resource {
    /// Where it sits in the package.
    pub fn entry(&self) -> String {
        format!("{ASSETS_FOLDER}/{}/{}", self.kind.folder(), self.name.as_str())
    }
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.resource({:?})", self.kind.folder(), self.name.as_str())
    }
}

/// The resources a script names as literals, `keyboard.resource("...")` or `host.resource("...")`, so
/// packing can check they exist. Names built at run time are checked when the script runs.
pub fn literal_resources(script: &str) -> Vec<(ResourceKind, String)> {
    let mut found = Vec::new();
    for kind in ResourceKind::ALL {
        let call = format!("{}.resource(", kind.folder());
        let mut rest = script;
        while let Some(at) = rest.find(&call) {
            rest = &rest[at + call.len()..];
            let quoted = rest.trim_start();
            let Some(quote) = quoted.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            if let Some(end) = quoted[1..].find(quote) {
                found.push((kind, quoted[1..1 + end].to_owned()));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_inside_the_module() {
        for good in ["bar.svg", "pet/idle-1.png", "a_b/c.d/e.json"] {
            assert!(ResourceName::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "/bar.svg", "pet//idle.png", "../x.png", "pet/../x.png", "./x.png", "Bar.svg", "a b.png", "c:x.png", "a\\b.png"] {
            assert!(ResourceName::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn finds_literal_names_with_either_quote() {
        let script = r#"local a = keyboard.resource("pet/idle.png") local b = host.resource( 'limits.json' ) local c = keyboard.resource(name)"#;
        assert_eq!(
            literal_resources(script),
            vec![(ResourceKind::Keyboard, "pet/idle.png".to_owned()), (ResourceKind::Host, "limits.json".to_owned())]
        );
    }
}
