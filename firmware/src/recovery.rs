//! Not in the stock firmware: a build that keeps crashing drops the chip into its USB boot ROM,
//! which can't be overwritten, so a fixed image can be flashed without opening the keyboard.
//! Panics and watchdog timeouts count in a watchdog scratch register, which survives those
//! resets; a power-on or a healthy run clears it.

use rp2040_hal::pac;

const CRASHES_TO_BOOTLOADER: u32 = 3;
/// How long the loop must run before earlier crashes are forgotten.
pub const HEALTHY_AFTER_MS: u32 = 10_000;
/// Longer than any loop pass, littlefs compaction included.
pub const WATCHDOG_TIMEOUT_US: u32 = 8_000_000;
const MAGIC: u32 = 0x6C61_7300;
const COUNT_MASK: u32 = 0xFF;

fn watchdog() -> &'static pac::watchdog::RegisterBlock {
    // SAFETY: SCRATCH0 belongs to this module; REASON is read-only.
    unsafe { &*pac::WATCHDOG::ptr() }
}

fn crashes() -> u32 {
    let word = watchdog().scratch0().read().bits();
    if word & !COUNT_MASK == MAGIC {
        word & COUNT_MASK
    } else {
        0
    }
}

fn set_crashes(count: u32) {
    watchdog()
        .scratch0()
        .write(|w| unsafe { w.bits(MAGIC | count.min(COUNT_MASK)) });
}

/// Call first at boot. Enters the USB boot ROM instead of returning after too many crashes.
pub fn check_boot() {
    let reason = watchdog().reason().read();
    let count = if reason.timer().bit_is_set() {
        crashes() + 1
    } else if reason.force().bit_is_set() {
        crashes()
    } else {
        0
    };
    if count >= CRASHES_TO_BOOTLOADER {
        enter_usb_boot();
    }
    set_crashes(count);
}

pub fn mark_healthy() {
    set_crashes(0);
}

/// Clears the count first, so the next image starts fresh.
pub fn enter_usb_boot() -> ! {
    set_crashes(0);
    rp2040_hal::rom_data::reset_to_usb_boot(0, 0);
    loop {
        cortex_m::asm::nop();
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    cortex_m::interrupt::disable();
    set_crashes(crashes() + 1);
    // SAFETY: resetting the chip; nothing else runs.
    let psm = unsafe { &*pac::PSM::ptr() };
    psm.wdsel().write(|w| {
        unsafe { w.bits(0x0001_FFFF) }
            .xosc()
            .clear_bit()
            .rosc()
            .clear_bit()
    });
    watchdog().ctrl().write(|w| w.trigger().set_bit());
    loop {
        cortex_m::asm::nop();
    }
}
