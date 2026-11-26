#![cfg_attr(not(test), no_std)]
#![allow(
    clippy::needless_range_loop,
    reason = "indexes mirror the C so the port can be checked line by line"
)]
//! The key scanner's signal processing, ported from Keyglass's C firmware (`keyboard/firmware`,
//! MIT OR Apache-2.0) to give the same output for the same input, down to every integer.
//! All signal quantities are calibrated flux counts.
//!
//! [`scanner::Runtime`] is the whole scan pass the firmware calls each sweep: frame detection
//! ([`keyframe`]), calibration and boot recovery ([`rest`], [`rest_session`]), drift ([`thermal`]),
//! key decisions, then the keymap, layers, macros and the HID report.
//!
//! Two paths turn a row of sensor readings into key decisions:
//! - classic (no press refinement): [`classic::condition_row`], [`classic::Neighbors`], [`classic::Keys`]
//! - refined: [`condition_row_q4`], [`depth::DepthRow`] and [`refine::RefineRow`], then per key
//!   [`rapid::RapidState`] or [`guard::fixed_candidate`]

pub mod calibration;
pub mod classic;
pub mod commands;
pub mod depth;
pub mod guard;
pub mod keyframe;
pub mod layout;
pub mod rapid;
pub mod refine;
mod refine_lut;
pub mod rest;
pub mod rest_session;
mod row_condition;
pub mod scanner;
pub mod thermal;

pub use row_condition::condition_row_q4;

/// The most sensors in a row.
pub const MAX_ROW_SENSORS: usize = 17;
/// The most keys in a row.
pub const MAX_ROW_KEYS: usize = 16;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod testing_lifecycle;
#[cfg(test)]
mod testing_runtime;
