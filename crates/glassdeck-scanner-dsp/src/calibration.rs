//! The factory calibration: one Q8 gain per sensor (Keyglass `fw_calibration.c`).

use crate::layout::SENSORS;

pub const MAGIC: u32 = 0xFAC5_CA1E;
pub const UNITY_Q8: u16 = 0x0100;
pub const INFO_LEN: usize = 20;
pub const FACTORS_LEN: usize = SENSORS * 2;
/// Version 2 factors are used multiplied by 0xAA / 0x100.
const V2_SCALE_Q8: u32 = 0xAA;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    pub loaded_from_flash: bool,
    pub version: u32,
    pub factors: [u16; SENSORS],
}

impl Default for Calibration {
    fn default() -> Self {
        Self {
            loaded_from_flash: false,
            version: 0,
            factors: [UNITY_Q8; SENSORS],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationError {
    Length,
    Magic(u32),
    SensorCount(u32),
    Checksum { stored: u32, computed: u32 },
}

impl Calibration {
    /// Reads `factory_calibration_info` (20 bytes) and `factory_scaling_factors` (320 bytes).
    pub fn load(info: &[u8], factors: &[u8]) -> Result<Self, CalibrationError> {
        if info.len() != INFO_LEN || factors.len() != FACTORS_LEN {
            return Err(CalibrationError::Length);
        }
        let word = |at: usize| u32::from_le_bytes(info[at..at + 4].try_into().unwrap());
        let (magic, version, sensors, stored) = (word(0), word(4), word(8), word(16));
        if magic != MAGIC {
            return Err(CalibrationError::Magic(magic));
        }
        if sensors as usize != SENSORS {
            return Err(CalibrationError::SensorCount(sensors));
        }
        let raw: [u16; SENSORS] =
            core::array::from_fn(|i| u16::from_le_bytes([factors[2 * i], factors[2 * i + 1]]));
        let computed = raw.iter().fold(0u32, |s, &f| s.wrapping_add(f as u32));
        if computed != stored {
            return Err(CalibrationError::Checksum { stored, computed });
        }
        let factors = raw.map(|f| {
            if version == 2 {
                ((f as u32 * V2_SCALE_Q8) >> 8) as u16
            } else {
                f
            }
        });
        Ok(Self {
            loaded_from_flash: true,
            version,
            factors,
        })
    }
}
