#![cfg_attr(not(test), no_std)]
//! Wire formats of the key scanner (the keyboard's RP2040), byte for byte as the stock firmware
//! speaks them. Ported from Keyglass (`keyboard/firmware`, MIT OR Apache-2.0); see `docs/protocol/uart.md` there.

pub mod hdlc;
pub mod serial;
pub mod usb;

#[cfg(test)]
mod testing {
    /// Lines of `name value` printed by a program in `firmware/vectors/`.
    pub(crate) struct Vectors(pub(crate) &'static str);

    impl Vectors {
        pub(crate) fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'static str> + 'a {
            self.0.lines().filter_map(move |line| {
                let (key, value) = line.split_once(' ').unwrap_or((line, ""));
                (key == name).then_some(value)
            })
        }

        pub(crate) fn get(&self, name: &str) -> &'static str {
            self.all(name)
                .next()
                .unwrap_or_else(|| panic!("no vector {name}"))
        }
    }

    pub(crate) fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub(crate) fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }
}
