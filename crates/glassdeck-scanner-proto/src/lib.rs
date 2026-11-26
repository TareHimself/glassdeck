#![cfg_attr(not(test), no_std)]
//! Wire formats of the key scanner (the keyboard's RP2040), byte for byte as the stock firmware
//! speaks them. Ported from Keyglass (`keyboard/firmware`, MIT OR Apache-2.0); see `docs/protocol/uart.md` there.

pub mod hdlc;
pub mod serial;
