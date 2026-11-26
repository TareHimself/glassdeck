//! The key scanner firmware: Keyglass's `rp2040_main.c` loop, one core, same order and timing.
//! Each pass services USB, the command interface and the serial link, sweeps the sensors,
//! runs `glassdeck_scanner_dsp::scanner::Runtime`, sends HID reports and the key state.
#![no_std]
#![no_main]
#![allow(
    clippy::needless_range_loop,
    reason = "indexes mirror the C so the port can be checked line by line"
)]

mod board;
mod link;
mod recovery;
mod storage;
mod usb;

use board::{RestMeasurement, Sweep};
use glassdeck_scanner_dsp::commands::{self, AfterAnswer, Storage, StoredRefinement};
use glassdeck_scanner_dsp::layout::SENSORS;
use glassdeck_scanner_dsp::refine::MODEL_LEN;
use glassdeck_scanner_dsp::scanner::{HidScheduler, HidSink, Runtime};
use glassdeck_scanner_proto::keymap::MAX_BLOB;
use glassdeck_scanner_proto::usb::{
    FIRMWARE_MAX_REQUEST, FIRMWARE_PACKET, REFINEMENT_HEADER_LEN, REFINEMENT_RECORD_LEN, Received,
    Receiver, Reply, Transmitter,
};
use glassdeck_scanner_proto::usb_descriptors::REPORT_ID_CONSUMER;
use rp2040_hal::fugit::{ExtU32, RateExtU32};
use rp2040_hal::uart::{DataBits, StopBits, UartConfig, UartPeripheral};
use rp2040_hal::usb::UsbBus;
use rp2040_hal::{Clock, Sio, Watchdog, clocks, gpio, pac};
use usb_device::class_prelude::UsbBusAllocator;

#[unsafe(link_section = ".boot2")]
#[used]
static BOOT2: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

const XOSC_HZ: u32 = 12_000_000;
const VERSION: &[u8] = b"glassdeck-scanner 0.0.1\0";
const BRIDGE_HEARTBEAT_MS: u32 = 1000;
/// A bridge counts as connected for three heartbeats after its last good response.
const BRIDGE_ACK_LIFETIME_US: u64 = 3 * BRIDGE_HEARTBEAT_MS as u64 * 1000;
const FULL_SWEEP_EVERY_MS: u32 = 16;
const VENDOR_PACKETS_PER_PASS: u32 = 4;
/// A command whose packets stop arriving for this long is dropped.
const COMMAND_IDLE_TIMEOUT_MS: u32 = 1000;
const BOOTLOADER_DELAY_MS: u32 = 50;
const COMPUTER_RESET_HOLD_MS: u32 = 200;

type CommandReceiver = Receiver<FIRMWARE_PACKET, FIRMWARE_MAX_REQUEST>;
type CommandTransmitter = Transmitter<FIRMWARE_PACKET, FIRMWARE_MAX_REQUEST>;

/// The keyboard endpoints as the HID scheduler sees them (Keyglass `rp2040_send_*`).
struct Endpoints<'s, 'a>(&'s mut usb::Scanner<'a>);

impl HidSink for Endpoints<'_, '_> {
    fn keyboard(&mut self, modifiers: u8, keycodes: &[u8; 6], nkro_bitmap: &[u8; 32]) -> bool {
        if self.0.keyboard_protocol() == usb::PROTOCOL_BOOT {
            let mut report = [0u8; 8];
            report[0] = modifiers;
            report[2..].copy_from_slice(keycodes);
            return self.0.send_keyboard(0, &report);
        }
        let mut report = [0u8; 33];
        report[1..].copy_from_slice(nkro_bitmap);
        self.0.send_keyboard(
            glassdeck_scanner_proto::usb_descriptors::REPORT_ID_KEYBOARD,
            &report,
        )
    }

    fn consumer(&mut self, usage: u16) -> bool {
        self.0
            .send_keyboard(REPORT_ID_CONSUMER, &usage.to_le_bytes())
    }

    fn functions(&mut self, mask: u32) -> bool {
        self.0.send_functions(&mask.to_le_bytes())
    }
}

/// What the command interface asked for, done once its answer is out.
#[derive(Default)]
struct Pending {
    enter_bootloader: bool,
    reset_computer: bool,
}

struct Commands {
    receiver: CommandReceiver,
    transmitter: CommandTransmitter,
    last_rx_ms: u32,
}

impl Commands {
    /// Keyglass `rp2040_usb_vendor_service`: one bounded pass that never waits on the endpoint.
    fn service(
        &mut self,
        now_ms: u32,
        scanner: &mut usb::Scanner<'_>,
        rt: &mut Runtime,
        store: &mut storage::Store,
        pending: &mut Pending,
    ) {
        self.transmitter
            .step(now_ms, VENDOR_PACKETS_PER_PASS, scanner);
        let mut packet = [0u8; FIRMWARE_PACKET];
        for _ in 0..VENDOR_PACKETS_PER_PASS {
            if self.transmitter.is_busy() || !scanner.commands_available() {
                break;
            }
            let n = scanner.read_commands(&mut packet);
            if n > 0 {
                self.feed(&packet[..n], rt, store, pending);
                self.last_rx_ms = now_ms;
            }
        }
        if self.transmitter.is_busy() {
            self.transmitter
                .step(now_ms, VENDOR_PACKETS_PER_PASS, scanner);
        }
        if self.receiver.is_busy()
            && now_ms.wrapping_sub(self.last_rx_ms) >= COMMAND_IDLE_TIMEOUT_MS
        {
            self.receiver.reset();
        }
    }

    fn feed(
        &mut self,
        packet: &[u8],
        rt: &mut Runtime,
        store: &mut storage::Store,
        pending: &mut Pending,
    ) {
        match self.receiver.push(packet) {
            Received::Pending => {}
            Received::Refused => self.transmitter.start(Reply::Nak, 0),
            Received::Request { command, payload } => {
                let answer = commands::answer(
                    rt,
                    store,
                    VERSION,
                    command,
                    payload,
                    self.transmitter.payload_mut(),
                );
                self.transmitter.start(answer.reply, answer.len);
                match answer.then {
                    AfterAnswer::Nothing => {}
                    AfterAnswer::EnterBootloader => pending.enter_bootloader = true,
                    AfterAnswer::ResetComputer => pending.reset_computer = true,
                }
            }
        }
    }

    fn reset(&mut self) {
        self.receiver.reset();
        self.transmitter.reset();
    }
}

/// Changes that make the bridge want a fresh key state even when no key moved.
#[derive(PartialEq, Eq)]
struct Reported {
    bitmap: [u8; 32],
    leds: u8,
    profile: u32,
    actuation_mode: u32,
    platform: u32,
    layers: u32,
    function_events: u16,
}

impl Reported {
    fn of(rt: &Runtime) -> Self {
        Self {
            bitmap: rt.key_bitmap,
            leds: rt.host_state_status,
            profile: rt.active_profile,
            actuation_mode: rt.active_actuation_mode,
            platform: rt.platform_code,
            layers: rt.effective_layer_bitfield,
            function_events: rt.of_event_seq,
        }
    }
}

fn reset_computer(link: &mut link::Link, bridge_ack_expires_us: &mut u64) {
    board::gpio_init(board::SOC_RESET);
    board::gpio_set_output(board::SOC_RESET);
    board::gpio_put(board::SOC_RESET, true);
    board::gpio_put(board::SOC_POWER, true);
    board::sleep_ms(COMPUTER_RESET_HOLD_MS);
    link.discard_input();
    *bridge_ack_expires_us = 0;
    board::gpio_put(board::SOC_RESET, false);
}

#[cortex_m_rt::entry]
fn main() -> ! {
    recovery::check_boot();
    let mut pac = pac::Peripherals::take().expect("taken once");
    let mut watchdog = Watchdog::new(pac.WATCHDOG);
    let clocks = clocks::init_clocks_and_plls(
        XOSC_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .unwrap_or_else(|_| panic!("the 12 MHz crystal didn't start"));
    let sio = Sio::new(pac.SIO);
    let pins = gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    board::init_pins();
    board::init_adc(&mut pac.RESETS);
    let uart_pins = (
        pins.gpio8.into_function::<gpio::FunctionUart>(),
        pins.gpio9.into_function::<gpio::FunctionUart>(),
    );
    let _uart = UartPeripheral::new(pac.UART1, uart_pins, &mut pac.RESETS)
        .enable(
            UartConfig::new(board::UART_BAUD.Hz(), DataBits::Eight, None, StopBits::One),
            clocks.peripheral_clock.freq(),
        )
        .expect("921600 baud divides the peripheral clock");
    let unique_id = storage::unique_id();

    let rt: &'static mut Runtime =
        cortex_m::singleton!(: Runtime = Runtime::default()).expect("taken once");
    let mut store = storage::Store::mount();
    let commands: &'static mut Commands = cortex_m::singleton!(: Commands = Commands {
        receiver: Receiver::default(),
        transmitter: Transmitter::default(),
        last_rx_ms: 0,
    })
    .expect("taken once");
    if store.is_mounted() {
        let scratch: &mut [u8; MAX_BLOB] = commands.transmitter.payload_mut();
        store.load_boot_state(rt, scratch);
        let record: &mut [u8; REFINEMENT_RECORD_LEN] =
            (&mut scratch[..REFINEMENT_RECORD_LEN]).try_into().unwrap();
        if store.read_refinement(record) == StoredRefinement::Installed {
            let model: &[u8; MODEL_LEN] = record[REFINEMENT_HEADER_LEN..].try_into().unwrap();
            rt.install_refinement(model);
        }
    }

    let bus: &'static UsbBusAllocator<UsbBus> = cortex_m::singleton!(: UsbBusAllocator<UsbBus> =
        UsbBusAllocator::new(UsbBus::new(pac.USBCTRL_REGS, pac.USBCTRL_DPRAM, clocks.usb_clock, true, &mut pac.RESETS)))
    .expect("taken once");
    let mut scanner = usb::Scanner::new(bus, &unique_id);
    let mut device = usb::Scanner::device(bus);
    let mut link = link::Link::new();

    let raw: &'static mut [u16; SENSORS] =
        cortex_m::singleton!(: [u16; SENSORS] = [0; SENSORS]).expect("taken once");
    let rest: &'static mut RestMeasurement =
        cortex_m::singleton!(: RestMeasurement = RestMeasurement::new()).expect("taken once");
    // With no frame identified yet these are only starting offsets; the first attached frame
    // gets its own measurement.
    board::measure_rest(raw, rest);
    rt.baseline_adc = rest.mean;

    watchdog.start(recovery::WATCHDOG_TIMEOUT_US.micros());
    let mut hid = HidScheduler::default();
    let mut pending = Pending::default();
    let mut bridge_ack_expires_us = 0u64;
    let mut reported = Reported::of(rt);
    let boot_ms = board::now_ms();
    let mut healthy = false;
    let mut last_full_ms = boot_ms;
    let mut last_uart_ms = boot_ms.wrapping_sub(BRIDGE_HEARTBEAT_MS);

    loop {
        watchdog.feed();
        device.poll(&mut [&mut scanner]);
        scanner.track_state(device.state());
        if let Some(leds) = scanner.take_leds() {
            rt.host_state_status = leds;
        }
        if scanner.take_config_changed() {
            // A new configuration knows nothing of earlier reports or a half-received command.
            hid = HidScheduler::default();
            commands.reset();
        }

        commands.service(board::now_ms(), &mut scanner, rt, &mut store, &mut pending);

        link.receive(|response| {
            if !response.rejected {
                bridge_ack_expires_us = board::now_us() + BRIDGE_ACK_LIFETIME_US;
            }
            rt.handle_response(response);
        });

        if pending.enter_bootloader && !commands.transmitter.is_busy() {
            board::sleep_ms(BOOTLOADER_DELAY_MS);
            recovery::enter_usb_boot();
        }
        if pending.reset_computer && !commands.transmitter.is_busy() {
            pending.reset_computer = false;
            reset_computer(&mut link, &mut bridge_ack_expires_us);
        }

        let scan_ms = board::now_ms();
        let mut full = scan_ms.wrapping_sub(last_full_ms) >= FULL_SWEEP_EVERY_MS;
        board::scan_sensors(raw, if full { Sweep::All } else { Sweep::Keys });
        if full {
            last_full_ms = scan_ms;
        }
        if rt.poll_frame(raw, scan_ms) {
            board::measure_rest(raw, rest);
            full = true;
            last_full_ms = board::now_ms();
            store.load_rest_reference(rt);
            let calibrated_ms = board::now_ms();
            let bridge_connected = board::now_us() < bridge_ack_expires_us;
            rt.calibrate(&rest.mean, &rest.unstable, calibrated_ms, bridge_connected);
        }
        let keyboard_ready = scanner.keyboard_ready();
        rt.set_time(scan_ms);
        rt.set_hid_ready(keyboard_ready);
        let report = rt.process_scan(raw);
        scanner.last_report = report;
        let now_ms = board::now_ms();
        if rt.finish_scan(now_ms) {
            store.save_rest_reference(rt);
        }
        if rt.platform_dirty {
            store.save_platform_code(rt.platform_code);
            rt.platform_dirty = false;
        }
        let functions_ready = scanner.functions_ready();
        hid.step(
            &report,
            keyboard_ready,
            functions_ready,
            &mut Endpoints(&mut scanner),
        );

        let now_reported = Reported::of(rt);
        if now_reported != reported || now_ms.wrapping_sub(last_uart_ms) >= BRIDGE_HEARTBEAT_MS {
            link.send(&rt.key_state(now_ms).encode());
            reported = now_reported;
            last_uart_ms = now_ms;
        }
        if rt.qc_mode && full {
            link.send(&rt.sensor_readings().encode());
        }

        if !healthy && now_ms.wrapping_sub(boot_ms) >= recovery::HEALTHY_AFTER_MS {
            recovery::mark_healthy();
            healthy = true;
        }
    }
}
