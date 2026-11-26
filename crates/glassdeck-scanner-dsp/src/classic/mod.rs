//! The model-free key tracker, used when press refinement is not active (Keyglass
//! `fw_classic_row.c`, `fw_classic_neighbors.c`, `fw_classic_keys.c`). Per scan and row:
//! [`condition_row`], then [`Neighbors::correct`], then [`Keys::step`].

mod keys;
mod neighbors;
mod row;

pub use keys::{Keys, Reversal, Settings};
pub use neighbors::{Neighbors, Thresholds};
pub use row::condition_row;

/// Keyframe layout IDs as the firmware numbers them.
pub const ISO: u8 = 2;
/// The bottom row, whose busy-row thresholds differ.
const SPACE_ROW: usize = 5;
const ROWS: usize = 6;

/// Actuation and release depth for a key with no actuation table.
pub const DEFAULT_ACTUATION: u16 = 650;
pub const DEFAULT_HYSTERESIS: u16 = 216;
