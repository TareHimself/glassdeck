#![cfg_attr(not(test), no_std)]
//! The key scanner's signal processing, ported from Keyglass's C firmware (`keyboard/firmware`,
//! MIT OR Apache-2.0) to give the same output for the same input, down to every integer.
//! All signal quantities are calibrated flux counts.
//!
//! Two paths turn a row of sensor readings into key decisions:
//! - classic (no press refinement): [`classic::condition_row`], [`classic::Neighbors`], [`classic::Keys`]
//! - refined: [`condition_row_q4`], then per key [`rapid::RapidState`] or [`guard::fixed_candidate`]

pub mod classic;
pub mod depth;
pub mod guard;
pub mod rapid;
pub mod refine;
mod refine_lut;
mod row_condition;

pub use row_condition::condition_row_q4;

/// The most sensors in a row.
pub const MAX_ROW_SENSORS: usize = 17;
/// The most keys in a row.
pub const MAX_ROW_KEYS: usize = 16;

#[cfg(test)]
mod testing;
