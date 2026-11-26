//! The composite USB device, answering as the stock firmware's TinyUSB setup does: descriptors
//! from `glassdeck_scanner_proto::usb_descriptors`, the HID, CDC and vendor class requests, and
//! TinyUSB's 256-byte stream FIFOs on the command interface.

use glassdeck_scanner_dsp::scanner::HidReport;
use glassdeck_scanner_proto::usb::{FIRMWARE_PACKET, PacketSink};
use glassdeck_scanner_proto::usb_descriptors as desc;
use rp2040_hal::usb::UsbBus;
use usb_device::class::{ControlIn, ControlOut, UsbClass};
use usb_device::class_prelude::{
    EndpointAddress, EndpointIn, EndpointOut, EndpointType, InterfaceNumber, UsbBusAllocator,
};
use usb_device::control::{Recipient, Request, RequestType};
use usb_device::device::{UsbDevice, UsbDeviceBuilder, UsbDeviceState, UsbVidPid};
use usb_device::endpoint::{In, Out};

type Bus = UsbBus;

const HID_GET_REPORT: u8 = 0x01;
const HID_GET_IDLE: u8 = 0x02;
const HID_GET_PROTOCOL: u8 = 0x03;
const HID_SET_REPORT: u8 = 0x09;
const HID_SET_IDLE: u8 = 0x0A;
const HID_SET_PROTOCOL: u8 = 0x0B;
const HID_REPORT_INPUT: u8 = 1;
const HID_REPORT_OUTPUT: u8 = 2;
const HID_REPORT_INVALID: u8 = 0;
const HID_EP_BUFSIZE: usize = 64;
pub const PROTOCOL_BOOT: u8 = 0;
const PROTOCOL_REPORT: u8 = 1;

const CDC_SET_LINE_CODING: u8 = 0x20;
const CDC_GET_LINE_CODING: u8 = 0x21;
const CDC_SET_CONTROL_LINE_STATE: u8 = 0x22;
const CDC_SEND_BREAK: u8 = 0x23;
/// TinyUSB's line coding before the host sets one: 115200 8N1.
const CDC_DEFAULT_LINE_CODING: [u8; 7] = [0x00, 0xC2, 0x01, 0x00, 0, 0, 8];

const DESC_DEVICE: u8 = 1;
const DESC_CONFIGURATION: u8 = 2;
const DESC_STRING: u8 = 3;
const DESC_BOS: u8 = 0x0F;
const DESC_HID: u8 = 0x21;
const DESC_HID_REPORT: u8 = 0x22;

const STREAM_FIFO: usize = 256;
const PACKET: usize = FIRMWARE_PACKET;

/// TinyUSB's endpoint stream FIFO.
struct Fifo {
    buf: [u8; STREAM_FIFO],
    head: usize,
    len: usize,
}

impl Fifo {
    const fn new() -> Self {
        Self {
            buf: [0; STREAM_FIFO],
            head: 0,
            len: 0,
        }
    }

    fn room(&self) -> usize {
        STREAM_FIFO - self.len
    }

    fn push(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.room());
        for &b in &data[..n] {
            self.buf[(self.head + self.len) % STREAM_FIFO] = b;
            self.len += 1;
        }
        n
    }

    fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for slot in &mut out[..n] {
            *slot = self.buf[self.head];
            self.head = (self.head + 1) % STREAM_FIFO;
            self.len -= 1;
        }
        n
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

/// One packet at a time on an IN endpoint, as `usbd_edpt_xfer` with a 64-byte buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InFlight {
    Idle,
    Packet { len: usize },
}

/// Everything the stock firmware exposes on USB, as one usb-device class.
pub struct Scanner<'a> {
    keyboard: EndpointIn<'a, Bus>,
    functions: EndpointIn<'a, Bus>,
    commands_out: EndpointOut<'a, Bus>,
    commands_in: EndpointIn<'a, Bus>,
    _cdc_notify: EndpointIn<'a, Bus>,
    cdc_out: EndpointOut<'a, Bus>,
    _cdc_in: EndpointIn<'a, Bus>,
    serial: [u8; 16],
    keyboard_in_flight: bool,
    functions_in_flight: bool,
    commands_in_flight: InFlight,
    rx: Fifo,
    tx: Fifo,
    /// TinyUSB's CDC receive FIFO, which nothing reads: once full, the endpoint is left unarmed.
    cdc_rx_len: usize,
    line_coding: [u8; 7],
    protocol: [u8; 2],
    idle_rate: [u8; 2],
    mounted: bool,
    /// Mounted and not suspended (`tud_ready`).
    ready: bool,
    /// Set by a mount, unmount or keyboard SET_PROTOCOL; taken by the main loop.
    config_changed: bool,
    /// An LED output report from the host; taken by the main loop.
    leds: Option<u8>,
    pub last_report: HidReport,
}

fn endpoint<'a, D: usb_device::endpoint::EndpointDirection>(
    alloc: &'a UsbBusAllocator<Bus>,
    address: u8,
    kind: EndpointType,
    packet: u16,
    interval: u8,
) -> usb_device::endpoint::Endpoint<'a, Bus, D> {
    alloc
        .alloc(Some(EndpointAddress::from(address)), kind, packet, interval)
        .expect("the stock endpoint layout fits the RP2040")
}

impl<'a> Scanner<'a> {
    pub fn new(alloc: &'a UsbBusAllocator<Bus>, unique_id: &[u8; 8]) -> Self {
        Self {
            keyboard: endpoint::<In>(
                alloc,
                desc::EP_KEYBOARD,
                EndpointType::Interrupt,
                desc::KEYBOARD_PACKET,
                1,
            ),
            commands_out: endpoint::<Out>(
                alloc,
                desc::EP_COMMANDS_OUT,
                EndpointType::Bulk,
                PACKET as u16,
                0,
            ),
            commands_in: endpoint::<In>(
                alloc,
                desc::EP_COMMANDS_IN,
                EndpointType::Bulk,
                PACKET as u16,
                0,
            ),
            _cdc_notify: endpoint::<In>(
                alloc,
                desc::EP_CDC_NOTIFY,
                EndpointType::Interrupt,
                desc::CDC_NOTIFY_PACKET,
                16,
            ),
            cdc_out: endpoint::<Out>(
                alloc,
                desc::EP_CDC_OUT,
                EndpointType::Bulk,
                desc::CDC_PACKET,
                0,
            ),
            _cdc_in: endpoint::<In>(
                alloc,
                desc::EP_CDC_IN,
                EndpointType::Bulk,
                desc::CDC_PACKET,
                0,
            ),
            functions: endpoint::<In>(
                alloc,
                desc::EP_FUNCTIONS,
                EndpointType::Interrupt,
                desc::FUNCTIONS_PACKET,
                1,
            ),
            serial: desc::serial_number(unique_id),
            keyboard_in_flight: false,
            functions_in_flight: false,
            commands_in_flight: InFlight::Idle,
            rx: Fifo::new(),
            tx: Fifo::new(),
            cdc_rx_len: 0,
            line_coding: CDC_DEFAULT_LINE_CODING,
            protocol: [PROTOCOL_BOOT; 2],
            idle_rate: [0; 2],
            mounted: false,
            ready: false,
            config_changed: false,
            leds: None,
            last_report: HidReport::default(),
        }
    }

    pub fn device(alloc: &'a UsbBusAllocator<Bus>) -> UsbDevice<'a, Bus> {
        UsbDeviceBuilder::new(
            alloc,
            UsbVidPid(
                glassdeck_scanner_proto::usb::VID,
                glassdeck_scanner_proto::usb::PID,
            ),
        )
        .max_packet_size_0(FIRMWARE_PACKET as u8)
        .expect("64 is a valid EP0 size")
        .supports_remote_wakeup(true)
        .max_power(100)
        .expect("100 mA is in range")
        .build()
    }

    /// TinyUSB's mount state, which a suspend doesn't change. Call after every poll.
    pub fn track_state(&mut self, state: UsbDeviceState) {
        let mounted = match state {
            UsbDeviceState::Configured => true,
            UsbDeviceState::Suspend => self.mounted,
            UsbDeviceState::Default | UsbDeviceState::Addressed => false,
        };
        if mounted != self.mounted {
            self.mounted = mounted;
            self.config_changed = true;
            self.open_interfaces();
        }
        self.ready = state == UsbDeviceState::Configured;
    }

    fn open_interfaces(&mut self) {
        self.protocol = [if self.mounted {
            PROTOCOL_REPORT
        } else {
            PROTOCOL_BOOT
        }; 2];
        self.idle_rate = [0; 2];
        self.keyboard_in_flight = false;
        self.functions_in_flight = false;
        self.commands_in_flight = InFlight::Idle;
        self.rx.clear();
        self.tx.clear();
        self.cdc_rx_len = 0;
    }

    pub fn take_config_changed(&mut self) -> bool {
        core::mem::take(&mut self.config_changed)
    }

    pub fn take_leds(&mut self) -> Option<u8> {
        self.leds.take()
    }

    pub fn keyboard_protocol(&self) -> u8 {
        self.protocol[0]
    }

    /// `tud_hid_n_ready`: mounted, not suspended, and the endpoint's last report taken.
    pub fn keyboard_ready(&self) -> bool {
        self.ready && !self.keyboard_in_flight
    }

    pub fn functions_ready(&self) -> bool {
        self.ready && !self.functions_in_flight
    }

    /// `tud_hid_n_report`: the report ID, if any, goes before the report.
    pub fn send_keyboard(&mut self, report_id: u8, report: &[u8]) -> bool {
        if !self.keyboard_ready() {
            return false;
        }
        let mut buf = [0u8; HID_EP_BUFSIZE];
        let len = with_report_id(&mut buf, report_id, report);
        let sent = self.keyboard.write(&buf[..len]).is_ok();
        self.keyboard_in_flight |= sent;
        sent
    }

    pub fn send_functions(&mut self, report: &[u8]) -> bool {
        if !self.functions_ready() {
            return false;
        }
        let sent = self.functions.write(report).is_ok();
        self.functions_in_flight |= sent;
        sent
    }

    /// Moves packets the host has sent into the receive FIFO while it has room for one.
    fn fill_rx(&mut self) {
        while self.rx.room() >= PACKET {
            let mut packet = [0u8; PACKET];
            match self.commands_out.read(&mut packet) {
                Ok(n) => {
                    self.rx.push(&packet[..n]);
                }
                Err(_) => break,
            }
        }
    }

    pub fn commands_available(&mut self) -> bool {
        self.fill_rx();
        self.rx.len > 0
    }

    /// `tud_vendor_read`: up to one packet's worth of the receive stream.
    pub fn read_commands(&mut self, out: &mut [u8; PACKET]) -> usize {
        self.fill_rx();
        let n = self.rx.pop(out);
        self.fill_rx();
        n
    }

    fn start_commands_in(&mut self) {
        if self.commands_in_flight != InFlight::Idle || self.tx.len == 0 || !self.ready {
            return;
        }
        let mut packet = [0u8; PACKET];
        let len = self.tx.pop(&mut packet);
        if self.commands_in.write(&packet[..len]).is_ok() {
            self.commands_in_flight = InFlight::Packet { len };
        }
    }

    fn drain_cdc(&mut self) {
        while self.cdc_rx_len < STREAM_FIFO {
            let mut packet = [0u8; PACKET];
            match self.cdc_out.read(&mut packet) {
                Ok(n) => self.cdc_rx_len += n,
                Err(_) => break,
            }
        }
    }

    fn hid_instance(index: u16) -> Option<usize> {
        match index as u8 {
            desc::INTERFACE_KEYBOARD => Some(0),
            desc::INTERFACE_FUNCTIONS => Some(1),
            _ => None,
        }
    }

    /// Keyglass's `fw_on_hid_get_report`: the bytes after any report ID TinyUSB adds.
    fn get_report(&self, instance: usize, report_id: u8, out: &mut [u8]) -> usize {
        let r = &self.last_report;
        if instance == 1 {
            if out.len() < 4 {
                return 0;
            }
            out[..4].copy_from_slice(&r.of_func_mask.to_le_bytes());
            return 4;
        }
        if self.protocol[0] == PROTOCOL_BOOT || report_id == 0 {
            if out.len() < 8 {
                return 0;
            }
            out[0] = r.modifiers;
            out[1] = 0;
            out[2..8].copy_from_slice(&r.keycodes);
            return 8;
        }
        if report_id == desc::REPORT_ID_KEYBOARD {
            if out.len() < 33 {
                return 0;
            }
            out[0] = 0;
            out[1..33].copy_from_slice(&r.nkro_bitmap);
            return 33;
        }
        0
    }

    fn standard_in(&mut self, xfer: ControlIn<Bus>) {
        let req = *xfer.request();
        let kind = (req.value >> 8) as u8;
        let index = req.value as u8;
        let _ = match (req.recipient, req.request, kind) {
            (Recipient::Device, Request::GET_DESCRIPTOR, DESC_DEVICE) => {
                xfer.accept_with_static(&desc::DEVICE)
            }
            (Recipient::Device, Request::GET_DESCRIPTOR, DESC_CONFIGURATION) => {
                xfer.accept_with_static(&desc::CONFIGURATION)
            }
            (Recipient::Device, Request::GET_DESCRIPTOR, DESC_BOS) => {
                xfer.accept_with_static(&desc::BOS)
            }
            (Recipient::Device, Request::GET_DESCRIPTOR, DESC_STRING) => {
                let mut buf = [0; desc::STRING_DESCRIPTOR_MAX];
                match desc::string(index, &self.serial, &mut buf) {
                    Some(s) => xfer.accept_with(s),
                    None => xfer.reject(),
                }
            }
            (Recipient::Device, Request::GET_DESCRIPTOR, _) => xfer.reject(),
            (Recipient::Interface, Request::GET_DESCRIPTOR, _) => {
                match (Self::hid_instance(req.index), kind) {
                    (Some(0), DESC_HID) => xfer.accept_with_static(&desc::KEYBOARD_HID),
                    (Some(1), DESC_HID) => xfer.accept_with_static(&desc::FUNCTIONS_HID),
                    (Some(0), DESC_HID_REPORT) => xfer.accept_with_static(&desc::KEYBOARD_REPORT),
                    (Some(1), DESC_HID_REPORT) => xfer.accept_with_static(&desc::FUNCTIONS_REPORT),
                    _ => xfer.reject(),
                }
            }
            _ => return,
        };
    }

    fn class_in(&mut self, xfer: ControlIn<Bus>) {
        let req = *xfer.request();
        if req.recipient != Recipient::Interface {
            return;
        }
        if req.index as u8 == desc::INTERFACE_CDC {
            let _ = match req.request {
                CDC_GET_LINE_CODING => xfer.accept_with(&self.line_coding),
                _ => xfer.reject(),
            };
            return;
        }
        let Some(hid) = Self::hid_instance(req.index) else {
            let _ = xfer.reject();
            return;
        };
        let _ = match req.request {
            HID_GET_REPORT => {
                let report_type = (req.value >> 8) as u8;
                let report_id = req.value as u8;
                let mut buf = [0u8; HID_EP_BUFSIZE];
                let mut room = (req.length as usize).min(HID_EP_BUFSIZE);
                let mut len = 0;
                if report_id != HID_REPORT_INVALID && room > 1 {
                    buf[0] = report_id;
                    room -= 1;
                    len = 1;
                }
                if report_type == HID_REPORT_INPUT {
                    len += self.get_report(hid, report_id, &mut buf[len..len + room]);
                }
                if len == 0 {
                    xfer.reject()
                } else {
                    xfer.accept_with(&buf[..len])
                }
            }
            HID_GET_IDLE => xfer.accept_with(&self.idle_rate[hid..hid + 1]),
            HID_GET_PROTOCOL => xfer.accept_with(&self.protocol[hid..hid + 1]),
            _ => xfer.reject(),
        };
    }

    fn class_out(&mut self, xfer: ControlOut<Bus>) {
        let req = *xfer.request();
        if req.recipient != Recipient::Interface {
            return;
        }
        if req.index as u8 == desc::INTERFACE_CDC {
            let _ = match req.request {
                CDC_SET_LINE_CODING if xfer.data().len() == 7 => {
                    self.line_coding.copy_from_slice(xfer.data());
                    xfer.accept()
                }
                CDC_SET_CONTROL_LINE_STATE | CDC_SEND_BREAK => xfer.accept(),
                _ => xfer.reject(),
            };
            return;
        }
        let Some(hid) = Self::hid_instance(req.index) else {
            let _ = xfer.reject();
            return;
        };
        let _ = match req.request {
            HID_SET_REPORT if xfer.data().len() <= HID_EP_BUFSIZE => {
                let report_type = (req.value >> 8) as u8;
                let report_id = req.value as u8;
                let mut report = xfer.data();
                if report_id != HID_REPORT_INVALID && report.len() > 1 && report[0] == report_id {
                    report = &report[1..];
                }
                // Keyglass's `tud_hid_set_report_cb`: the keyboard's LED byte.
                if hid == 0
                    && (report_type == HID_REPORT_OUTPUT || report_type == HID_REPORT_INVALID)
                    && (report_id == 0 || report_id == desc::REPORT_ID_KEYBOARD)
                    && !report.is_empty()
                {
                    self.leds = Some(report[0]);
                }
                xfer.accept()
            }
            HID_SET_IDLE => {
                self.idle_rate[hid] = (req.value >> 8) as u8;
                xfer.accept()
            }
            HID_SET_PROTOCOL => {
                self.protocol[hid] = req.value as u8;
                if hid == 0 {
                    self.config_changed = true;
                }
                xfer.accept()
            }
            _ => xfer.reject(),
        };
    }
}

fn with_report_id(buf: &mut [u8; HID_EP_BUFSIZE], report_id: u8, report: &[u8]) -> usize {
    if report_id == 0 {
        buf[..report.len()].copy_from_slice(report);
        report.len()
    } else {
        buf[0] = report_id;
        buf[1..=report.len()].copy_from_slice(report);
        report.len() + 1
    }
}

impl UsbClass<Bus> for Scanner<'_> {
    fn reset(&mut self) {
        // TinyUSB's unmount callback only fires for a device that was mounted.
        self.config_changed |= self.mounted;
        self.mounted = false;
        self.ready = false;
        self.open_interfaces();
    }

    fn control_in(&mut self, xfer: ControlIn<Bus>) {
        let req = *xfer.request();
        match req.request_type {
            RequestType::Standard => self.standard_in(xfer),
            RequestType::Class => self.class_in(xfer),
            // TinyUSB hands every vendor request to `tud_vendor_control_xfer_cb`.
            RequestType::Vendor => {
                let _ = if req.request == desc::MS_VENDOR_CODE && req.index == desc::MS_OS_20_INDEX
                {
                    xfer.accept_with_static(&desc::MS_OS_20)
                } else {
                    xfer.reject()
                };
            }
            RequestType::Reserved => {}
        }
    }

    fn control_out(&mut self, xfer: ControlOut<Bus>) {
        let req = *xfer.request();
        match req.request_type {
            RequestType::Class => self.class_out(xfer),
            RequestType::Vendor => {
                let _ = xfer.reject();
            }
            RequestType::Standard | RequestType::Reserved => {}
        }
    }

    fn get_alt_setting(&mut self, interface: InterfaceNumber) -> Option<u8> {
        (u8::from(interface) <= desc::INTERFACE_FUNCTIONS).then_some(0)
    }

    fn set_alt_setting(&mut self, interface: InterfaceNumber, alternative: u8) -> bool {
        u8::from(interface) <= desc::INTERFACE_FUNCTIONS && alternative == 0
    }

    fn endpoint_out(&mut self, addr: EndpointAddress) {
        if addr == self.commands_out.address() {
            self.fill_rx();
        } else if addr == self.cdc_out.address() {
            self.drain_cdc();
        }
    }

    fn endpoint_in_complete(&mut self, addr: EndpointAddress) {
        if addr == self.keyboard.address() {
            self.keyboard_in_flight = false;
        } else if addr == self.functions.address() {
            self.functions_in_flight = false;
        } else if addr == self.commands_in.address() {
            let InFlight::Packet { len } = self.commands_in_flight else {
                return;
            };
            self.commands_in_flight = InFlight::Idle;
            if self.tx.len > 0 {
                self.start_commands_in();
            } else if len == PACKET {
                // TinyUSB ends a stream that drained on a full packet with a zero-length one.
                if self.commands_in.write(&[]).is_ok() {
                    self.commands_in_flight = InFlight::Packet { len: 0 };
                }
            }
        }
    }
}

impl PacketSink<PACKET> for Scanner<'_> {
    fn space(&mut self) -> usize {
        self.tx.room()
    }

    /// `tud_vendor_write`, which starts a transfer once a packet's worth is queued.
    fn write(&mut self, packet: &[u8; PACKET]) -> bool {
        let written = self.tx.push(packet);
        if self.tx.len >= PACKET {
            self.start_commands_in();
        }
        written == PACKET
    }

    fn flush(&mut self) {
        self.start_commands_in();
    }
}
