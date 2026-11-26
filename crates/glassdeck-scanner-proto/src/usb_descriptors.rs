//! The key scanner's USB descriptors, byte for byte as the stock firmware serves them: a composite
//! device with the keyboard (HID, boot capable), the command interface (vendor bulk, bound to WinUSB
//! through Microsoft OS 2.0 descriptors), a CDC serial port and a vendor HID for function keys.

use crate::usb::{FIRMWARE_PACKET, PID, VID};

pub const INTERFACE_KEYBOARD: u8 = 0;
pub const INTERFACE_COMMANDS: u8 = 1;
pub const INTERFACE_CDC: u8 = 2;
pub const INTERFACE_CDC_DATA: u8 = 3;
pub const INTERFACE_FUNCTIONS: u8 = 4;
const INTERFACES: u8 = 5;

pub const EP_COMMANDS_OUT: u8 = 0x01;
pub const EP_CDC_OUT: u8 = 0x02;
pub const EP_CDC_NOTIFY: u8 = 0x81;
pub const EP_CDC_IN: u8 = 0x82;
pub const EP_KEYBOARD: u8 = 0x83;
pub const EP_COMMANDS_IN: u8 = 0x84;
pub const EP_FUNCTIONS: u8 = 0x85;

pub const KEYBOARD_PACKET: u16 = 64;
pub const FUNCTIONS_PACKET: u16 = 8;
pub const CDC_PACKET: u16 = 64;
pub const CDC_NOTIFY_PACKET: u16 = 8;
const CDC_NOTIFY_INTERVAL: u8 = 16;

pub const REPORT_ID_KEYBOARD: u8 = 1;
pub const REPORT_ID_CONSUMER: u8 = 2;

pub const STRING_LANGUAGES: u8 = 0;
pub const STRING_MANUFACTURER: u8 = 1;
pub const STRING_PRODUCT: u8 = 2;
pub const STRING_SERIAL: u8 = 3;
pub const STRING_CDC: u8 = 4;
const LANGUAGE_EN_US: u16 = 0x0409;
const MANUFACTURER: &str = "OpenFlux";
const PRODUCT: &str = "OpenFlux Firmware";
const CDC_NAME: &str = "OpenFlux Firmware CDC";
/// The stock firmware truncates every string to this many characters.
const STRING_MAX_CHARS: usize = 31;

/// `bRequest` of the vendor control request that fetches [`MS_OS_20`].
pub const MS_VENDOR_CODE: u8 = 0x02;
/// `wIndex` of that request (MS_OS_20_DESCRIPTOR_INDEX).
pub const MS_OS_20_INDEX: u16 = 7;

const DESC_DEVICE: u8 = 0x01;
const DESC_CONFIGURATION: u8 = 0x02;
const DESC_STRING: u8 = 0x03;
const DESC_INTERFACE: u8 = 0x04;
const DESC_ENDPOINT: u8 = 0x05;
const DESC_BOS: u8 = 0x0F;
const DESC_DEVICE_CAPABILITY: u8 = 0x10;
const DESC_HID: u8 = 0x21;
const DESC_HID_REPORT: u8 = 0x22;
const DESC_CS_INTERFACE: u8 = 0x24;
const DESC_IAD: u8 = 0x0B;

const CLASS_MISC: u8 = 0xEF;
const CLASS_CDC: u8 = 0x02;
const CLASS_HID: u8 = 0x03;
const CLASS_CDC_DATA: u8 = 0x0A;
const CLASS_VENDOR: u8 = 0xFF;
const HID_SUBCLASS_BOOT: u8 = 1;
const HID_PROTOCOL_KEYBOARD: u8 = 1;
const CDC_SUBCLASS_ACM: u8 = 2;

const EP_BULK: u8 = 0x02;
const EP_INTERRUPT: u8 = 0x03;

/// Keyboard interface: report 1 is a full NKRO bitmap (one reserved byte, 256 usage bits, five LED
/// outputs); report 2 is a 16-bit consumer usage. Boot-protocol hosts get the 8-byte boot report.
pub const KEYBOARD_REPORT: [u8; 76] = [
    0x05,
    0x01,
    0x09,
    0x06,
    0xA1,
    0x01,
    0x85,
    REPORT_ID_KEYBOARD, //
    0x95,
    0x01,
    0x75,
    0x08,
    0x81,
    0x01, //
    0x05,
    0x07,
    0x19,
    0x00,
    0x2A,
    0xFF,
    0x00,
    0x15,
    0x00,
    0x25,
    0x01, //
    0x96,
    0x00,
    0x01,
    0x75,
    0x01,
    0x81,
    0x02, //
    0x05,
    0x08,
    0x19,
    0x01,
    0x29,
    0x05,
    0x95,
    0x05,
    0x75,
    0x01,
    0x91,
    0x02, //
    0x95,
    0x01,
    0x75,
    0x03,
    0x91,
    0x01,
    0xC0, //
    0x05,
    0x0C,
    0x09,
    0x01,
    0xA1,
    0x01,
    0x85,
    REPORT_ID_CONSUMER, //
    0x15,
    0x00,
    0x26,
    0xFF,
    0x03,
    0x19,
    0x00,
    0x2A,
    0xFF,
    0x03, //
    0x95,
    0x01,
    0x75,
    0x10,
    0x81,
    0x00,
    0xC0,
];

/// Function-key interface: a 32-bit mask, no report ID, so host software can read it without the
/// OS asking for keyboard-monitoring permission.
pub const FUNCTIONS_REPORT: [u8; 25] = [
    0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, //
    0x06, 0x00, 0xFF, 0x19, 0x00, 0x29, 0x1F, 0x15, 0x00, 0x25, 0x01, //
    0x75, 0x01, 0x95, 0x20, 0x81, 0x02, 0xC0,
];

pub const DEVICE: [u8; 18] = Bytes::new()
    .put(&[18, DESC_DEVICE])
    .u16(0x0210)
    .put(&[CLASS_MISC, 0x02, 0x01, FIRMWARE_PACKET as u8])
    .u16(VID)
    .u16(PID)
    .u16(0x0101)
    .put(&[STRING_MANUFACTURER, STRING_PRODUCT, STRING_SERIAL, 1])
    .finish();

const CONFIGURATION_LEN: usize = 148;

pub const CONFIGURATION: [u8; CONFIGURATION_LEN] = Bytes::new()
    .put(&[9, DESC_CONFIGURATION])
    .u16(CONFIGURATION_LEN as u16)
    .put(&[INTERFACES, 1, 0, 0xA0, 100 / 2])
    .put(&interface(
        INTERFACE_KEYBOARD,
        1,
        CLASS_HID,
        HID_SUBCLASS_BOOT,
        HID_PROTOCOL_KEYBOARD,
        0,
    ))
    .put(&hid(KEYBOARD_REPORT.len()))
    .put(&endpoint(EP_KEYBOARD, EP_INTERRUPT, KEYBOARD_PACKET, 1))
    .put(&interface(INTERFACE_COMMANDS, 2, CLASS_VENDOR, 0, 0, 0))
    .put(&endpoint(
        EP_COMMANDS_OUT,
        EP_BULK,
        FIRMWARE_PACKET as u16,
        0,
    ))
    .put(&endpoint(
        EP_COMMANDS_IN,
        EP_BULK,
        FIRMWARE_PACKET as u16,
        0,
    ))
    .put(&[
        8,
        DESC_IAD,
        INTERFACE_CDC,
        2,
        CLASS_CDC,
        CDC_SUBCLASS_ACM,
        0,
        0,
    ])
    .put(&interface(
        INTERFACE_CDC,
        1,
        CLASS_CDC,
        CDC_SUBCLASS_ACM,
        0,
        STRING_CDC,
    ))
    .put(&[5, DESC_CS_INTERFACE, 0x00, 0x20, 0x01])
    .put(&[5, DESC_CS_INTERFACE, 0x01, 0x00, INTERFACE_CDC_DATA])
    .put(&[4, DESC_CS_INTERFACE, 0x02, 0x06])
    .put(&[
        5,
        DESC_CS_INTERFACE,
        0x06,
        INTERFACE_CDC,
        INTERFACE_CDC_DATA,
    ])
    .put(&endpoint(
        EP_CDC_NOTIFY,
        EP_INTERRUPT,
        CDC_NOTIFY_PACKET,
        CDC_NOTIFY_INTERVAL,
    ))
    .put(&interface(INTERFACE_CDC_DATA, 2, CLASS_CDC_DATA, 0, 0, 0))
    .put(&endpoint(EP_CDC_OUT, EP_BULK, CDC_PACKET, 0))
    .put(&endpoint(EP_CDC_IN, EP_BULK, CDC_PACKET, 0))
    .put(&interface(INTERFACE_FUNCTIONS, 1, CLASS_HID, 0, 0, 0))
    .put(&hid(FUNCTIONS_REPORT.len()))
    .put(&endpoint(EP_FUNCTIONS, EP_INTERRUPT, FUNCTIONS_PACKET, 1))
    .finish();

/// The class-specific HID descriptor that follows a HID interface, as [`CONFIGURATION`] holds it.
pub const KEYBOARD_HID: [u8; 9] = hid(KEYBOARD_REPORT.len());
pub const FUNCTIONS_HID: [u8; 9] = hid(FUNCTIONS_REPORT.len());

const MS_OS_20_LEN: usize = 0xB2;
const MS_OS_20_UUID: [u8; 16] = [
    0xDF, 0x60, 0xDD, 0xD8, 0x89, 0x45, 0xC7, 0x4C, 0x9C, 0xD2, 0x65, 0x9D, 0x9E, 0x64, 0x8A, 0x9F,
];
const WINDOWS_8_1: u32 = 0x0603_0000;

pub const BOS: [u8; 33] = Bytes::new()
    .put(&[5, DESC_BOS])
    .u16(33)
    .put(&[1])
    .put(&[28, DESC_DEVICE_CAPABILITY, 0x05, 0])
    .put(&MS_OS_20_UUID)
    .u32(WINDOWS_8_1)
    .u16(MS_OS_20_LEN as u16)
    .put(&[MS_VENDOR_CODE, 0])
    .finish();

const COMMANDS_INTERFACE_GUID: &str = "{8E9A3F42-5C71-4B0E-9D26-4F1A2B3C4D5E}";

/// Binds WinUSB to the command interface and gives it a fixed interface GUID.
pub const MS_OS_20: [u8; MS_OS_20_LEN] = Bytes::new()
    .u16(0x0A)
    .u16(0x00)
    .u32(WINDOWS_8_1)
    .u16(MS_OS_20_LEN as u16)
    .u16(0x08)
    .u16(0x01)
    .put(&[0, 0])
    .u16(MS_OS_20_LEN as u16 - 0x0A)
    .u16(0x08)
    .u16(0x02)
    .put(&[INTERFACE_COMMANDS, 0])
    .u16(MS_OS_20_LEN as u16 - 0x0A - 0x08)
    .u16(0x14)
    .u16(0x03)
    .put(b"WINUSB\0\0")
    .put(&[0; 8])
    .u16(MS_OS_20_LEN as u16 - 0x0A - 0x08 - 0x08 - 0x14)
    .u16(0x04)
    .u16(0x07)
    .u16(0x2A)
    .utf16z("DeviceInterfaceGUIDs")
    .u16(0x50)
    .utf16z(COMMANDS_INTERFACE_GUID)
    .put(&[0, 0])
    .finish();

pub const STRING_DESCRIPTOR_MAX: usize = 2 + 2 * STRING_MAX_CHARS;

/// The serial number: the flash chip's 64-bit unique ID in upper-case hex.
pub fn serial_number(unique_id: &[u8; 8]) -> [u8; 16] {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = [0; 16];
    for (i, b) in unique_id.iter().enumerate() {
        out[2 * i] = HEX[(b >> 4) as usize];
        out[2 * i + 1] = HEX[(b & 15) as usize];
    }
    out
}

/// String descriptor `index` (UTF-16LE, any language ID), or `None` past the last one.
pub fn string<'a>(
    index: u8,
    serial: &[u8; 16],
    out: &'a mut [u8; STRING_DESCRIPTOR_MAX],
) -> Option<&'a [u8]> {
    let text: &[u8] = match index {
        STRING_LANGUAGES => {
            out[..2].copy_from_slice(&[4, DESC_STRING]);
            out[2..4].copy_from_slice(&LANGUAGE_EN_US.to_le_bytes());
            return Some(&out[..4]);
        }
        STRING_MANUFACTURER => MANUFACTURER.as_bytes(),
        STRING_PRODUCT => PRODUCT.as_bytes(),
        STRING_SERIAL => serial,
        STRING_CDC => CDC_NAME.as_bytes(),
        _ => return None,
    };
    let chars = text.len().min(STRING_MAX_CHARS);
    let len = 2 + 2 * chars;
    out[..2].copy_from_slice(&[len as u8, DESC_STRING]);
    for (i, &c) in text[..chars].iter().enumerate() {
        out[2 + 2 * i..4 + 2 * i].copy_from_slice(&(c as u16).to_le_bytes());
    }
    Some(&out[..len])
}

const fn interface(
    number: u8,
    endpoints: u8,
    class: u8,
    subclass: u8,
    protocol: u8,
    string: u8,
) -> [u8; 9] {
    [
        9,
        DESC_INTERFACE,
        number,
        0,
        endpoints,
        class,
        subclass,
        protocol,
        string,
    ]
}

const fn endpoint(address: u8, kind: u8, packet: u16, interval: u8) -> [u8; 7] {
    let p = packet.to_le_bytes();
    [7, DESC_ENDPOINT, address, kind, p[0], p[1], interval]
}

const fn hid(report_len: usize) -> [u8; 9] {
    let len = (report_len as u16).to_le_bytes();
    [
        9,
        DESC_HID,
        0x11,
        0x01,
        0,
        1,
        DESC_HID_REPORT,
        len[0],
        len[1],
    ]
}

/// Builds a descriptor at compile time; [`Bytes::finish`] checks it filled exactly `N` bytes.
struct Bytes<const N: usize> {
    out: [u8; N],
    len: usize,
}

impl<const N: usize> Bytes<N> {
    const fn new() -> Self {
        Self {
            out: [0; N],
            len: 0,
        }
    }

    const fn put(mut self, bytes: &[u8]) -> Self {
        let mut i = 0;
        while i < bytes.len() {
            self.out[self.len + i] = bytes[i];
            i += 1;
        }
        self.len += bytes.len();
        self
    }

    const fn u16(self, value: u16) -> Self {
        self.put(&value.to_le_bytes())
    }

    const fn u32(self, value: u32) -> Self {
        self.put(&value.to_le_bytes())
    }

    /// An ASCII string as UTF-16LE with a terminating NUL.
    const fn utf16z(mut self, text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            self = self.put(&[bytes[i], 0]);
            i += 1;
        }
        self.put(&[0, 0])
    }

    const fn finish(self) -> [u8; N] {
        assert!(self.len == N);
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Vectors, hex};

    const VECTORS: Vectors = Vectors(include_str!(
        "../../../firmware/vectors/usb_descriptors.txt"
    ));

    #[test]
    fn descriptors_match_the_stock_firmware() {
        for (name, bytes) in [
            ("device", &DEVICE[..]),
            ("configuration", &CONFIGURATION[..]),
            ("hid_report.0", &KEYBOARD_REPORT[..]),
            ("hid_report.1", &FUNCTIONS_REPORT[..]),
            ("bos", &BOS[..]),
            ("ms_os_20", &MS_OS_20[..]),
        ] {
            assert_eq!(hex(bytes), VECTORS.get(name), "{name}");
        }
    }

    #[test]
    fn strings_match_the_stock_firmware() {
        let serial = serial_number(&[0xE6, 0x61, 0x41, 0x03, 0xE7, 0x45, 0x2D, 0x2F]);
        let mut buf = [0; STRING_DESCRIPTOR_MAX];
        for index in 0..6u8 {
            let got = string(index, &serial, &mut buf).map_or("none".into(), hex);
            assert_eq!(
                got,
                VECTORS.get(&format!("string.{index}")),
                "string {index}"
            );
        }
    }

    #[test]
    fn the_hid_descriptors_are_the_ones_in_the_configuration() {
        let at = |needle: &[u8]| {
            CONFIGURATION
                .windows(needle.len())
                .position(|w| w == needle)
        };
        assert!(at(&KEYBOARD_HID).is_some());
        assert!(at(&FUNCTIONS_HID).is_some());
    }
}
