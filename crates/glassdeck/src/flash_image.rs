//! The key scanner's littlefs partition inside a flash dump, read with the firmware's own
//! littlefs and configuration, so a backup can be checked before anything is flashed.

use std::ffi::{CString, c_char};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use glassdeck_scanner_dsp::calibration::{Calibration, FACTORS_LEN, INFO_LEN};

pub const PARTITION_OFFSET: usize = 0x00F0_0000;
const BLOCK_SIZE: usize = 4096;
const BLOCK_COUNT: u32 = 256;
/// Some units were formatted one block short; the firmware mounts either.
const SHORT_BLOCK_COUNT: u32 = 255;
pub const PARTITION_LEN: usize = BLOCK_COUNT as usize * BLOCK_SIZE;
pub const CALIBRATION_INFO: &str = "factory_calibration_info";
pub const CALIBRATION_FACTORS: &str = "factory_scaling_factors";
const LFS_ERR_NOENT: i32 = -2;

unsafe extern "C" {
    fn gd_image_mount(image: *mut u8, blocks: u32) -> i32;
    fn gd_image_unmount() -> i32;
    fn gd_image_read(path: *const c_char, buffer: *mut u8, capacity: u32) -> i32;
}

#[cfg(test)]
unsafe extern "C" {
    fn gd_image_format(image: *mut u8, blocks: u32) -> i32;
    fn gd_image_write(path: *const c_char, data: *const u8, len: u32) -> i32;
}

/// The C side keeps one littlefs instance.
static LFS: Mutex<()> = Mutex::new(());

/// The factory calibration files, as stored, after checking they load.
pub struct FactoryCalibration {
    pub info: Vec<u8>,
    pub factors: Vec<u8>,
    pub calibration: Calibration,
}

/// Reads the factory calibration from a 1 MiB littlefs partition.
pub fn factory_calibration(partition: &[u8]) -> Result<FactoryCalibration> {
    let files = read_files(partition, &[CALIBRATION_INFO, CALIBRATION_FACTORS])?;
    let [info, factors] =
        <[Option<Vec<u8>>; 2]>::try_from(files).expect("two names in, two files out");
    let info = info.with_context(|| format!("the partition has no {CALIBRATION_INFO}"))?;
    let factors = factors.with_context(|| format!("the partition has no {CALIBRATION_FACTORS}"))?;
    let calibration = Calibration::load(&info, &factors).map_err(|e| {
        anyhow::anyhow!(
            "the factory calibration doesn't check out ({e:?}; {} and {} bytes, expected {INFO_LEN} and {FACTORS_LEN})",
            info.len(),
            factors.len()
        )
    })?;
    Ok(FactoryCalibration {
        info,
        factors,
        calibration,
    })
}

/// Each named file's contents, or `None` where it doesn't exist. Mounts as the firmware does:
/// 256 blocks, then 255.
pub fn read_files(partition: &[u8], names: &[&str]) -> Result<Vec<Option<Vec<u8>>>> {
    if partition.len() != PARTITION_LEN {
        bail!(
            "a partition is {PARTITION_LEN} bytes, not {}",
            partition.len()
        );
    }
    let _guard = LFS.lock().expect("no littlefs call panics");
    let mut image = partition.to_vec();
    // SAFETY: `image` outlives the mount, which ends before this returns.
    let mounted = unsafe {
        gd_image_mount(image.as_mut_ptr(), BLOCK_COUNT) == 0
            || gd_image_mount(image.as_mut_ptr(), SHORT_BLOCK_COUNT) == 0
    };
    if !mounted {
        bail!("the partition doesn't mount as littlefs");
    }
    let result = names.iter().map(|name| read_one(name)).collect();
    // SAFETY: mounted above.
    unsafe { gd_image_unmount() };
    result
}

fn read_one(name: &str) -> Result<Option<Vec<u8>>> {
    let path = CString::new(name).context("a file name with a NUL")?;
    let mut buf = vec![0u8; PARTITION_LEN];
    // SAFETY: a mounted image and a buffer of `buf.len()` bytes.
    let n = unsafe { gd_image_read(path.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    match n {
        LFS_ERR_NOENT => Ok(None),
        n if n < 0 => bail!("reading {name} from the partition failed (littlefs error {n})"),
        n => {
            buf.truncate(n as usize);
            Ok(Some(buf))
        }
    }
}

/// A partition holding `files`, for tests.
#[cfg(test)]
pub fn build_partition(files: &[(&str, &[u8])]) -> Vec<u8> {
    let _guard = LFS.lock().unwrap();
    let mut image = vec![0u8; PARTITION_LEN];
    unsafe {
        assert_eq!(gd_image_format(image.as_mut_ptr(), BLOCK_COUNT), 0);
        assert_eq!(gd_image_mount(image.as_mut_ptr(), BLOCK_COUNT), 0);
        for (name, data) in files {
            let path = CString::new(*name).unwrap();
            assert_eq!(
                gd_image_write(path.as_ptr(), data.as_ptr(), data.len() as u32),
                0
            );
        }
        assert_eq!(gd_image_unmount(), 0);
    }
    image
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use glassdeck_scanner_dsp::calibration::MAGIC;
    use glassdeck_scanner_dsp::layout::SENSORS;

    /// A valid version 1 calibration: info and factors.
    pub fn calibration_files() -> (Vec<u8>, Vec<u8>) {
        let factors: Vec<u8> = (0..SENSORS)
            .flat_map(|i| (0x100 + i as u16).to_le_bytes())
            .collect();
        let sum = (0..SENSORS).map(|i| 0x100 + i as u32).sum::<u32>();
        let mut info = Vec::new();
        for word in [MAGIC, 1, SENSORS as u32, 0, sum] {
            info.extend_from_slice(&word.to_le_bytes());
        }
        (info, factors)
    }

    #[test]
    fn the_factory_calibration_is_read_and_checked() {
        let (info, factors) = calibration_files();
        let partition = build_partition(&[
            (CALIBRATION_INFO, &info),
            (CALIBRATION_FACTORS, &factors),
            ("keymap", b"km"),
        ]);
        let cal = factory_calibration(&partition).unwrap();
        assert_eq!((cal.info, cal.factors), (info, factors));
        assert!(cal.calibration.loaded_from_flash);
        assert_eq!(cal.calibration.factors[1], 0x101);
    }

    #[test]
    fn a_missing_or_corrupt_calibration_is_refused() {
        let (info, mut factors) = calibration_files();
        let partition = build_partition(&[(CALIBRATION_INFO, &info)]);
        assert!(factory_calibration(&partition).is_err());
        factors[0] ^= 1;
        let partition =
            build_partition(&[(CALIBRATION_INFO, &info), (CALIBRATION_FACTORS, &factors)]);
        assert!(factory_calibration(&partition).is_err());
        assert!(factory_calibration(&vec![0xFF; PARTITION_LEN]).is_err());
    }
}
