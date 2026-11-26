//! The littlefs partition in the top 1 MiB of flash (Keyglass `lfs_store.c`): factory
//! calibration, keymap, profiles, settings, rest references and the press-refinement model.
//! The factory calibration files can't be remade, so nothing here can write them, and the
//! image itself ends below the partition (`memory.x`).

use core::ffi::{CStr, c_char, c_void};

use glassdeck_scanner_dsp::calibration::{Calibration, FACTORS_LEN, INFO_LEN};
use glassdeck_scanner_dsp::commands::{Storage, StoredRefinement};
use glassdeck_scanner_dsp::rest;
use glassdeck_scanner_dsp::scanner::Runtime;
use glassdeck_scanner_proto::keymap::{MAX_BLOB, ProfileSettings};
use glassdeck_scanner_proto::usb::{REFINEMENT_RECORD_LEN, RefinementRecord};

pub const PARTITION_OFFSET: u32 = 0x00F0_0000;
const BLOCK_SIZE: u32 = 4096;
const BLOCK_COUNT: u32 = 256;
/// Some units were formatted one block short; the stock store mounts either.
const SHORT_BLOCK_COUNT: u32 = 255;
const XIP_NOCACHE_NOALLOC: u32 = 0x1300_0000;

const LFS_ERR_OK: i32 = 0;
const LFS_ERR_IO: i32 = -5;
const LFS_ERR_NOENT: i32 = -2;
const LFS_ERR_ISDIR: i32 = -21;
const LFS_O_RDONLY: i32 = 1;
const LFS_O_WRONLY: i32 = 2;
const LFS_O_CREAT: i32 = 0x0100;
const LFS_O_TRUNC: i32 = 0x0400;

const CALIBRATION_INFO: &CStr = c"factory_calibration_info";
const CALIBRATION_FACTORS: &CStr = c"factory_scaling_factors";
const KEYMAP: &CStr = c"keymap";
const PROFILE_SETTINGS: &CStr = c"profile_settings_v1";
const LANGUAGE: &CStr = c"language_code";
const PLATFORM: &CStr = c"platform_type_code";
const REFINEMENT: &CStr = c"press_refinement_v1";
const REFINEMENT_STAGE: &CStr = c"press_refinement_v1.tmp";
/// The profile upload's file header, which the stored file leaves out.
const PROFILE_HEADER: usize = 16;

unsafe extern "C" {
    fn gd_lfs_mount(block_count: u32) -> i32;
    fn gd_lfs_open(path: *const c_char, flags: i32) -> i32;
    fn gd_lfs_size() -> i32;
    fn gd_lfs_read(buffer: *mut c_void, size: u32) -> i32;
    fn gd_lfs_write(buffer: *const c_void, size: u32) -> i32;
    fn gd_lfs_close() -> i32;
    fn gd_lfs_rename(from: *const c_char, to: *const c_char) -> i32;
    fn gd_lfs_remove(path: *const c_char) -> i32;
}

fn block_range(block: u32, off: u32, size: u32) -> Option<u32> {
    (block < BLOCK_COUNT && off + size <= BLOCK_SIZE)
        .then_some(PARTITION_OFFSET + block * BLOCK_SIZE + off)
}

#[unsafe(no_mangle)]
extern "C" fn gd_flash_read(block: u32, off: u32, buffer: *mut u8, size: u32) -> i32 {
    let Some(addr) = block_range(block, off, size) else {
        return LFS_ERR_IO;
    };
    // SAFETY: the uncached XIP alias of a range inside the partition; littlefs owns `buffer`.
    unsafe {
        core::ptr::copy_nonoverlapping(
            (XIP_NOCACHE_NOALLOC + addr) as *const u8,
            buffer,
            size as usize,
        )
    };
    LFS_ERR_OK
}

#[unsafe(no_mangle)]
extern "C" fn gd_flash_prog(block: u32, off: u32, buffer: *const u8, size: u32) -> i32 {
    let Some(addr) = block_range(block, off, size) else {
        return LFS_ERR_IO;
    };
    // SAFETY: littlefs programs whole 256-byte pages inside the partition from a RAM buffer.
    // One core and no interrupts run, so nothing reads flash while XIP is off.
    cortex_m::interrupt::free(|_| unsafe {
        let data = core::slice::from_raw_parts(buffer, size as usize);
        rp2040_flash::flash::flash_range_program(addr, data, true)
    });
    LFS_ERR_OK
}

#[unsafe(no_mangle)]
extern "C" fn gd_flash_erase(block: u32) -> i32 {
    let Some(addr) = block_range(block, 0, BLOCK_SIZE) else {
        return LFS_ERR_IO;
    };
    // SAFETY: one 4 KiB sector inside the partition, as above.
    cortex_m::interrupt::free(|_| unsafe {
        rp2040_flash::flash::flash_range_erase(addr, BLOCK_SIZE, true)
    });
    LFS_ERR_OK
}

/// Reads the flash chip's 64-bit unique ID, the USB serial number.
pub fn unique_id() -> [u8; 8] {
    let mut id = [0; 8];
    // SAFETY: called once at boot, before anything else uses flash or interrupts.
    cortex_m::interrupt::free(|_| unsafe { rp2040_flash::flash::flash_unique_id(&mut id, true) });
    id
}

/// The mounted file system. Without one every read fails and every write is refused, as
/// in the stock store.
pub struct Store {
    mounted: bool,
}

impl Store {
    /// Never formats: a failed mount must not wipe the factory calibration.
    pub fn mount() -> Self {
        // SAFETY: the glue keeps littlefs's state; this is the only mount.
        let mounted = unsafe {
            gd_lfs_mount(BLOCK_COUNT) == LFS_ERR_OK || gd_lfs_mount(SHORT_BLOCK_COUNT) == LFS_ERR_OK
        };
        Self { mounted }
    }

    pub fn is_mounted(&self) -> bool {
        self.mounted
    }

    /// The whole file, if it exists and fits `buf`.
    fn read_exact(&mut self, name: &CStr, buf: &mut [u8]) -> Option<usize> {
        if !self.mounted {
            return None;
        }
        // SAFETY: one file open at a time, closed before returning.
        unsafe {
            if gd_lfs_open(name.as_ptr(), LFS_O_RDONLY) != LFS_ERR_OK {
                return None;
            }
            let size = gd_lfs_size();
            if size < 0 || size as usize > buf.len() {
                gd_lfs_close();
                return None;
            }
            let read = gd_lfs_read(buf.as_mut_ptr().cast(), size as u32);
            gd_lfs_close();
            (read == size).then_some(read as usize)
        }
    }

    fn write_file(&mut self, name: &CStr, data: &[u8]) -> bool {
        if !self.mounted || name == CALIBRATION_INFO || name == CALIBRATION_FACTORS {
            return false;
        }
        // SAFETY: as in `read_exact`.
        unsafe {
            if gd_lfs_open(name.as_ptr(), LFS_O_WRONLY | LFS_O_CREAT | LFS_O_TRUNC) != LFS_ERR_OK {
                return false;
            }
            let written = gd_lfs_write(data.as_ptr().cast(), data.len() as u32);
            let closed = gd_lfs_close();
            written == data.len() as i32 && closed == LFS_ERR_OK
        }
    }

    fn read_u32(&mut self, name: &CStr) -> Option<u32> {
        let mut b = [0; 4];
        (self.read_exact(name, &mut b) == Some(4)).then(|| u32::from_le_bytes(b))
    }

    fn save_u32(&mut self, name: &CStr, value: u32) -> bool {
        self.write_file(name, &value.to_le_bytes())
    }

    pub fn save_platform_code(&mut self, code: u32) -> bool {
        self.save_u32(PLATFORM, code)
    }

    /// Loads calibration, keymap, profiles, language and platform into `rt`. Files that are
    /// missing or don't check out leave the defaults, as in the stock firmware. `scratch` holds
    /// the keymap while it loads.
    pub fn load_boot_state(&mut self, rt: &mut Runtime, scratch: &mut [u8; MAX_BLOB]) {
        if !self.mounted {
            return;
        }
        let mut info = [0; INFO_LEN];
        let mut factors = [0; FACTORS_LEN];
        if let (Some(i), Some(f)) = (
            self.read_exact(CALIBRATION_INFO, &mut info),
            self.read_exact(CALIBRATION_FACTORS, &mut factors),
        ) && let Ok(cal) = Calibration::load(&info[..i], &factors[..f])
        {
            rt.cal = cal;
        }
        if let Some(len) = self.read_exact(KEYMAP, scratch) {
            // A stored keymap that doesn't parse leaves the board with none, as stock.
            let _ = rt.load_keymap(&scratch[..len]);
        }
        let mut upload = [0; ProfileSettings::MAX_UPLOAD_LEN];
        if let Some(len) = self.read_exact(PROFILE_SETTINGS, &mut upload[PROFILE_HEADER..])
            && len >= PROFILE_HEADER
        {
            // Stored settings that don't parse leave the defaults, as stock.
            let _ = rt.load_profile_settings(&upload[..PROFILE_HEADER + len]);
        }
        if let Some(code) = self.read_u32(LANGUAGE) {
            rt.language_code = code;
        }
        if let Some(code) = self.read_u32(PLATFORM) {
            rt.platform_code = code;
        }
    }

    pub fn save_rest_reference(&mut self, rt: &Runtime) -> bool {
        if !rt.keyframe_attached || !rt.rest_ref_valid {
            return false;
        }
        let record = rest::encode(rt.keyframe_layout, rt.switch_feel, &rt.rest_ref_adc);
        self.write_file(
            rest_reference_name(rt.keyframe_layout, rt.switch_feel).as_c(),
            &record,
        )
    }

    /// Loads the rest reference for the attached layout and feel; clears it when there is none.
    pub fn load_rest_reference(&mut self, rt: &mut Runtime) {
        rt.rest_ref_valid = false;
        if !rt.keyframe_attached {
            return;
        }
        let mut record = [0; rest::RECORD_LEN];
        let name = rest_reference_name(rt.keyframe_layout, rt.switch_feel);
        if self.read_exact(name.as_c(), &mut record) != Some(rest::RECORD_LEN) {
            return;
        }
        if let Some(reference) = rest::decode(&record, rt.keyframe_layout, rt.switch_feel) {
            rt.rest_ref_adc = reference;
            rt.rest_ref_valid = true;
        }
    }

    fn refinement_file_matches(&mut self, name: &CStr, record: &[u8]) -> bool {
        // SAFETY: as in `read_exact`.
        unsafe {
            if gd_lfs_open(name.as_ptr(), LFS_O_RDONLY) != LFS_ERR_OK {
                return false;
            }
            let mut matches = gd_lfs_size() == REFINEMENT_RECORD_LEN as i32;
            let mut page = [0u8; 64];
            let mut at = 0;
            while matches && at < REFINEMENT_RECORD_LEN {
                let n = (REFINEMENT_RECORD_LEN - at).min(page.len());
                matches = gd_lfs_read(page.as_mut_ptr().cast(), n as u32) == n as i32
                    && page[..n] == record[at..at + n];
                at += n;
            }
            gd_lfs_close() == LFS_ERR_OK && matches
        }
    }
}

/// `rest_reference_<layout>_<feel>`, each a single digit.
struct RestReferenceName([u8; 20]);

impl RestReferenceName {
    fn as_c(&self) -> &CStr {
        CStr::from_bytes_until_nul(&self.0).expect("the name is NUL-terminated")
    }
}

fn rest_reference_name(layout: u8, feel: u8) -> RestReferenceName {
    let mut name = [0; 20];
    name[..15].copy_from_slice(b"rest_reference_");
    name[15] = b'0' + layout % 10;
    name[16] = b'_';
    name[17] = b'0' + feel % 10;
    RestReferenceName(name)
}

impl Storage for Store {
    fn save_keymap(&mut self, blob: &[u8]) -> bool {
        self.write_file(KEYMAP, blob)
    }

    fn save_profile_settings(&mut self, upload: &[u8]) -> bool {
        upload.len() >= 2 * PROFILE_HEADER
            && self.write_file(PROFILE_SETTINGS, &upload[PROFILE_HEADER..])
    }

    fn save_language(&mut self, code: u32) -> bool {
        self.save_u32(LANGUAGE, code)
    }

    fn save_platform(&mut self, code: u32) -> bool {
        self.save_platform_code(code)
    }

    fn read_refinement(&mut self, record: &mut [u8; REFINEMENT_RECORD_LEN]) -> StoredRefinement {
        if !self.mounted {
            return StoredRefinement::Unavailable;
        }
        // SAFETY: as in `read_exact`.
        unsafe {
            match gd_lfs_open(REFINEMENT.as_ptr(), LFS_O_RDONLY) {
                LFS_ERR_OK => {}
                LFS_ERR_NOENT => return StoredRefinement::Absent,
                LFS_ERR_ISDIR => return StoredRefinement::Invalid,
                _ => return StoredRefinement::Unavailable,
            }
            let size = gd_lfs_size();
            let state = if size == REFINEMENT_RECORD_LEN as i32 {
                if gd_lfs_read(record.as_mut_ptr().cast(), size as u32) == size {
                    if RefinementRecord::parse(record).is_ok() {
                        StoredRefinement::Installed
                    } else {
                        StoredRefinement::Invalid
                    }
                } else {
                    StoredRefinement::Unavailable
                }
            } else if size >= 0 {
                StoredRefinement::Invalid
            } else {
                StoredRefinement::Unavailable
            };
            if gd_lfs_close() == LFS_ERR_OK {
                state
            } else {
                StoredRefinement::Unavailable
            }
        }
    }

    /// Writes and verifies a staging file, then renames it over the live one in one littlefs
    /// transaction. A reset before the rename leaves a stage that's never loaded.
    fn install_refinement(&mut self, record: &RefinementRecord<'_>) -> bool {
        if !self.mounted {
            return false;
        }
        let bytes = record.bytes();
        if self.refinement_file_matches(REFINEMENT, bytes) {
            return true;
        }
        // SAFETY: no file is open here.
        let installed = self.write_file(REFINEMENT_STAGE, bytes)
            && self.refinement_file_matches(REFINEMENT_STAGE, bytes)
            && unsafe { gd_lfs_rename(REFINEMENT_STAGE.as_ptr(), REFINEMENT.as_ptr()) }
                == LFS_ERR_OK;
        if !installed {
            // SAFETY: as above.
            unsafe { gd_lfs_remove(REFINEMENT_STAGE.as_ptr()) };
        }
        installed
    }
}
