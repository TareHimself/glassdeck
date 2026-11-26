//! The scanner board's pins and the sensor sweep (Keyglass `rp2040_main.c` `init_hardware`,
//! `scan_sensors`, `measure_rest`), with the stock firmware's register access and timing.

use glassdeck_scanner_dsp::layout::{KEY_SENSORS, SENSORS};
use rp2040_hal::pac;

pub const SOC_AUX: u8 = 3;
pub const SOC_POWER: u8 = 4;
pub const SOC_RESET: u8 = 5;
pub const UART_BAUD: u32 = 921_600;
const ADC_PIN: usize = 29;
const ADC_CHANNEL: u8 = 3;

/// GPIO 10..17 pick one of 16 columns as a differential pair: bit `n` of the column on GPIO
/// 13 - n, and its inverse on GPIO 17 - n.
const MUX_MASK: u32 = 0x0003_FC00;
const BANKS: usize = 10;
const COLUMNS_PER_BANK: usize = 16;
/// Active-low bank selects, in sensor order.
const BANK_PINS: [u8; BANKS] = [18, 20, 21, 22, 24, 26, 27, 19, 23, 25];
const MUX_SETTLE_US: u32 = 2;

const REST_SWEEPS: u16 = 8;
const REST_SWEEP_GAP_MS: u32 = 2;
/// A key sensor whose readings span more than this over a rest measurement is unstable.
const REST_UNSTABLE_SPAN: u16 = 40;

const fn mux_value(column: usize) -> u32 {
    let c = (column & 15) as u32;
    let n = !c & 15;
    let value = (c & 1) << 13
        | (c >> 1 & 1) << 12
        | (c >> 2 & 1) << 11
        | (c >> 3 & 1) << 10
        | (n & 1) << 17
        | (n >> 1 & 1) << 16
        | (n >> 2 & 1) << 15
        | (n >> 3 & 1) << 14;
    value & MUX_MASK
}

const MUX_VALUES: [u32; COLUMNS_PER_BANK] = {
    let mut v = [0; COLUMNS_PER_BANK];
    let mut c = 0;
    while c < COLUMNS_PER_BANK {
        v[c] = mux_value(c);
        c += 1;
    }
    v
};

#[inline(always)]
fn sio() -> &'static pac::sio::RegisterBlock {
    // SAFETY: the SIO GPIO registers are only driven from this one core's foreground loop.
    unsafe { &*pac::SIO::ptr() }
}

#[inline(always)]
fn adc() -> &'static pac::adc::RegisterBlock {
    // SAFETY: only the sweep uses the ADC, after `init_adc`.
    unsafe { &*pac::ADC::ptr() }
}

#[inline(always)]
fn timer() -> &'static pac::timer::RegisterBlock {
    // SAFETY: the timer counters are read-only here.
    unsafe { &*pac::TIMER::ptr() }
}

#[inline(always)]
pub fn gpio_put(pin: u8, high: bool) {
    let bit = 1 << pin;
    if high {
        sio().gpio_out_set().write(|w| unsafe { w.bits(bit) });
    } else {
        sio().gpio_out_clr().write(|w| unsafe { w.bits(bit) });
    }
}

/// SDK `gpio_init`: input, output latch low, SIO function.
pub fn gpio_init(pin: u8) {
    // SAFETY: pins are configured before the loop starts, and the SoC reset pin again inside
    // it, never concurrently.
    let (io, pads) = unsafe { (&*pac::IO_BANK0::ptr(), &*pac::PADS_BANK0::ptr()) };
    let bit = 1 << pin;
    sio().gpio_oe_clr().write(|w| unsafe { w.bits(bit) });
    sio().gpio_out_clr().write(|w| unsafe { w.bits(bit) });
    pads.gpio(pin as usize)
        .modify(|_, w| w.ie().set_bit().od().clear_bit());
    io.gpio(pin as usize)
        .gpio_ctrl()
        .write(|w| w.funcsel().sio());
}

pub fn gpio_set_output(pin: u8) {
    sio().gpio_oe_set().write(|w| unsafe { w.bits(1 << pin) });
}

pub fn init_pins() {
    for pin in [SOC_AUX, SOC_POWER] {
        gpio_init(pin);
        gpio_set_output(pin);
        gpio_put(pin, true);
    }
    gpio_init(SOC_RESET);
    gpio_set_output(SOC_RESET);
    gpio_put(SOC_RESET, false);
    for pin in 10..=17 {
        gpio_init(pin);
        gpio_set_output(pin);
    }
    // Break before make: every bank deselected, then park on the first between sweeps.
    for &pin in &BANK_PINS {
        gpio_init(pin);
        gpio_put(pin, true);
        gpio_set_output(pin);
    }
    gpio_put(BANK_PINS[0], false);
}

pub fn init_adc(resets: &mut pac::RESETS) {
    resets.reset().modify(|_, w| w.adc().set_bit());
    resets.reset().modify(|_, w| w.adc().clear_bit());
    while resets.reset_done().read().adc().bit_is_clear() {}
    adc().cs().write(|w| w.en().set_bit());
    while adc().cs().read().ready().bit_is_clear() {}
    // SAFETY: GPIO 29 belongs to the ADC alone.
    let (io, pads) = unsafe { (&*pac::IO_BANK0::ptr(), &*pac::PADS_BANK0::ptr()) };
    io.gpio(ADC_PIN).gpio_ctrl().write(|w| w.funcsel().null());
    pads.gpio(ADC_PIN).modify(|_, w| {
        w.od()
            .clear_bit()
            .pue()
            .clear_bit()
            .pde()
            .clear_bit()
            .ie()
            .clear_bit()
    });
    adc()
        .cs()
        .modify(|_, w| unsafe { w.ainsel().bits(ADC_CHANNEL) });
}

pub fn now_us() -> u64 {
    loop {
        let hi = timer().timerawh().read().bits();
        let lo = timer().timerawl().read().bits();
        if timer().timerawh().read().bits() == hi {
            return (hi as u64) << 32 | lo as u64;
        }
    }
}

pub fn now_ms() -> u32 {
    (now_us() / 1000) as u32
}

#[inline(always)]
fn busy_wait_us(us: u32) {
    let start = timer().timerawl().read().bits();
    while timer().timerawl().read().bits().wrapping_sub(start) < us {}
}

pub fn sleep_ms(ms: u32) {
    let end = now_us() + ms as u64 * 1000;
    while now_us() < end {}
}

/// Which sensors a sweep reads.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sweep {
    /// The 112 key sensors the keyframe uses.
    Keys,
    /// All 160, including the frame and module sensors.
    All,
}

impl Sweep {
    fn count(self) -> usize {
        match self {
            Sweep::Keys => KEY_SENSORS,
            Sweep::All => SENSORS,
        }
    }
}

/// The atomic-set alias (+0x2000) of ADC CS at 0x4004C000.
const ADC_CS_SET: *mut u32 = 0x4004_E000 as *mut u32;
const ADC_START_ONCE: u32 = 1 << 2;

/// Reads `sweep`'s sensors into `raw`. Bank zero is selected on entry and exit; at a bank
/// boundary the old bank is released before the new one is selected. Runs from RAM, as the
/// stock sweep does, so flash cache misses can't stretch its timing.
#[inline(never)]
#[unsafe(link_section = ".data.ram_func")]
pub fn scan_sensors(raw: &mut [u16; SENSORS], sweep: Sweep) {
    let count = sweep.count();
    let sio = sio();
    let adc = adc();
    let put_masked = |value: u32| {
        let out = sio.gpio_out().read().bits();
        sio.gpio_out_xor()
            .write(|w| unsafe { w.bits((out ^ value) & MUX_MASK) });
    };
    let mut bank = 0;
    put_masked(MUX_VALUES[0]);
    busy_wait_us(MUX_SETTLE_US);
    // SAFETY: the atomic-set alias of ADC CS; setting START_ONCE begins one conversion.
    unsafe { ADC_CS_SET.write_volatile(ADC_START_ONCE) };
    for slot in 0..count {
        let next = slot + 1;
        if next < count {
            let next_bank = next / COLUMNS_PER_BANK;
            if next_bank != bank {
                gpio_put(BANK_PINS[bank], true);
                bank = next_bank;
                gpio_put(BANK_PINS[bank], false);
            }
            // The ADC converts this slot while the next input settles.
            put_masked(MUX_VALUES[next % COLUMNS_PER_BANK]);
        }
        while adc.cs().read().ready().bit_is_clear() {}
        let reading = adc.result().read().bits() as u16;
        if next < count {
            unsafe { ADC_CS_SET.write_volatile(ADC_START_ONCE) };
        }
        raw[slot] = reading;
    }
    if bank != 0 {
        gpio_put(BANK_PINS[bank], true);
        gpio_put(BANK_PINS[0], false);
    }
}

pub struct RestMeasurement {
    pub mean: [u16; SENSORS],
    pub unstable: [bool; KEY_SENSORS],
}

impl RestMeasurement {
    pub const fn new() -> Self {
        Self {
            mean: [0; SENSORS],
            unstable: [false; KEY_SENSORS],
        }
    }
}

/// Averages eight full sweeps 2 ms apart into `rest`, marking key sensors that moved over 40.
/// `raw` keeps the last real sweep, not the average.
pub fn measure_rest(raw: &mut [u16; SENSORS], rest: &mut RestMeasurement) {
    const { assert!(REST_SWEEPS as u32 * 4095 <= u16::MAX as u32) };
    let mut low = [0u16; KEY_SENSORS];
    let mut high = [0u16; KEY_SENSORS];
    rest.mean = [0; SENSORS];
    for pass in 0..REST_SWEEPS {
        scan_sensors(raw, Sweep::All);
        for i in 0..SENSORS {
            rest.mean[i] += raw[i];
            if i < KEY_SENSORS {
                if pass == 0 || raw[i] < low[i] {
                    low[i] = raw[i];
                }
                if pass == 0 || raw[i] > high[i] {
                    high[i] = raw[i];
                }
            }
        }
        sleep_ms(REST_SWEEP_GAP_MS);
    }
    for m in &mut rest.mean {
        *m /= REST_SWEEPS;
    }
    for i in 0..KEY_SENSORS {
        rest.unstable[i] = high[i] - low[i] > REST_UNSTABLE_SPAN;
    }
}
