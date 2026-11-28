//! The receiver's own USB function (FunctionFS), which bridges the Link between the PC and the
//! receiver's TCP listener. This is its core: the descriptors and strings written to ep0, the events
//! read back from it, and what each event makes the bridge do. The I/O lives in the platform module.
//! Layouts follow linux/usb/functionfs.h and linux/usb/ch9.h.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use anyhow::{Result, bail};
use glassdeck_proto::usb::{
    DEVICE_INTERFACE_GUID, FULL_SPEED_PACKET, HIGH_SPEED_PACKET, INTERFACE_CLASS, INTERFACE_PROTOCOL, INTERFACE_SUBCLASS,
    NEW_SESSION,
};

const DESCRIPTORS_MAGIC_V2: u32 = 3;
const STRINGS_MAGIC: u32 = 2;
const HAS_FS_DESC: u32 = 1;
const HAS_HS_DESC: u32 = 2;
const HAS_MS_OS_DESC: u32 = 8;

const INTERFACE_DESCRIPTOR: u8 = 4;
const ENDPOINT_DESCRIPTOR: u8 = 5;
const BULK: u8 = 2;
/// Endpoint numbers in order of appearance; FunctionFS names the files ep1 and ep2 after them, and the
/// gadget picks the addresses the PC sees.
pub const OUT_ENDPOINT: u8 = 0x01;
pub const IN_ENDPOINT: u8 = 0x82;
const INTERFACE_STRING: u8 = 1;

/// The kernel's f_fs accepts 0x0001 everywhere (newer kernels warn and prefer 0x0100).
const OS_DESC_VERSION: u16 = 0x0001;
const OS_DESC_EXT_COMPAT: u16 = 4;
const OS_DESC_EXT_PROP: u16 = 5;
const OS_DESC_HEADER_LEN: u32 = 11;
const EXT_COMPAT_LEN: u32 = 24;
const REG_MULTI_SZ: u32 = 7;

const ENGLISH_US: u16 = 0x0409;
const INTERFACE_NAME: &str = "Glassdeck";

/// One `struct usb_functionfs_event`: an 8-byte SETUP packet, the type, and 3 bytes of padding.
pub const EVENT_LEN: usize = 12;
/// SETUP packets to our interface, as FunctionFS hands them over: host to device, vendor, interface.
const VENDOR_OUT_TO_INTERFACE: u8 = 0x41;
const DEVICE_TO_HOST: u8 = 0x80;

/// What ep0 takes first: FS and HS descriptors for one vendor interface with a bulk endpoint each
/// way, and Microsoft OS descriptors so Windows binds WinUSB and programs can open it by GUID.
pub fn descriptors() -> Vec<u8> {
    let mut body = Vec::new();
    for packet in [FULL_SPEED_PACKET, HIGH_SPEED_PACKET] {
        body.extend(interface_descriptor());
        body.extend(endpoint_descriptor(OUT_ENDPOINT, packet));
        body.extend(endpoint_descriptor(IN_ENDPOINT, packet));
    }
    body.extend(ext_compat());
    body.extend(ext_prop());

    let counts: [u32; 3] = [3, 3, 2];
    let header_len = 12 + 4 * counts.len();
    let mut blob = Vec::with_capacity(header_len + body.len());
    blob.extend(DESCRIPTORS_MAGIC_V2.to_le_bytes());
    blob.extend(((header_len + body.len()) as u32).to_le_bytes());
    blob.extend((HAS_FS_DESC | HAS_HS_DESC | HAS_MS_OS_DESC).to_le_bytes());
    for count in counts {
        blob.extend(count.to_le_bytes());
    }
    blob.extend(body);
    blob
}

/// What ep0 takes second: the interface's name, in US English.
pub fn strings() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(ENGLISH_US.to_le_bytes());
    body.extend(INTERFACE_NAME.as_bytes());
    body.push(0);
    let mut blob = Vec::with_capacity(16 + body.len());
    blob.extend(STRINGS_MAGIC.to_le_bytes());
    blob.extend(((16 + body.len()) as u32).to_le_bytes());
    blob.extend(1u32.to_le_bytes());
    blob.extend(1u32.to_le_bytes());
    blob.extend(body);
    blob
}

fn interface_descriptor() -> [u8; 9] {
    [9, INTERFACE_DESCRIPTOR, 0, 0, 2, INTERFACE_CLASS, INTERFACE_SUBCLASS, INTERFACE_PROTOCOL, INTERFACE_STRING]
}

fn endpoint_descriptor(address: u8, packet: u16) -> [u8; 7] {
    let [low, high] = packet.to_le_bytes();
    [7, ENDPOINT_DESCRIPTOR, address, BULK, low, high, 0]
}

/// `struct usb_os_desc_header` for interface 0: its length counts the header itself.
fn os_desc_header(len: u32, index: u16, count: [u8; 2]) -> Vec<u8> {
    let mut header = vec![0];
    header.extend((OS_DESC_HEADER_LEN + len).to_le_bytes());
    header.extend(OS_DESC_VERSION.to_le_bytes());
    header.extend(index.to_le_bytes());
    header.extend(count);
    header
}

/// One `struct usb_ext_compat_desc` saying "WINUSB". The kernel requires Reserved1 to be 1.
fn ext_compat() -> Vec<u8> {
    let mut desc = os_desc_header(EXT_COMPAT_LEN, OS_DESC_EXT_COMPAT, [1, 0]);
    desc.extend([0, 1]);
    desc.extend(*b"WINUSB\0\0");
    desc.extend([0; 8]);
    desc.extend([0; 6]);
    desc
}

/// One extended property: DeviceInterfaceGUIDs, a REG_MULTI_SZ holding our GUID.
fn ext_prop() -> Vec<u8> {
    let name = utf16_with_nul("DeviceInterfaceGUIDs");
    let mut data = utf16_with_nul(DEVICE_INTERFACE_GUID);
    data.extend([0, 0]);
    let size = 14 + name.len() as u32 + data.len() as u32;
    let mut desc = os_desc_header(size, OS_DESC_EXT_PROP, 1u16.to_le_bytes());
    desc.extend(size.to_le_bytes());
    desc.extend(REG_MULTI_SZ.to_le_bytes());
    desc.extend((name.len() as u16).to_le_bytes());
    desc.extend(name);
    desc.extend((data.len() as u32).to_le_bytes());
    desc.extend(data);
    desc
}

fn utf16_with_nul(text: &str) -> Vec<u8> {
    text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    /// FunctionFS acks a request by reading or writing ep0 in its direction, and stalls it by doing
    /// the opposite.
    pub fn device_to_host(&self) -> bool {
        self.request_type & DEVICE_TO_HOST != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Bind,
    Unbind,
    Enable,
    Disable,
    Setup(Setup),
    Suspend,
    Resume,
}

pub fn decode_event(bytes: &[u8; EVENT_LEN]) -> Result<Event> {
    let word = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    Ok(match bytes[8] {
        0 => Event::Bind,
        1 => Event::Unbind,
        2 => Event::Enable,
        3 => Event::Disable,
        4 => Event::Setup(Setup { request_type: bytes[0], request: bytes[1], value: word(2), index: word(4), length: word(6) }),
        5 => Event::Suspend,
        6 => Event::Resume,
        other => bail!("FunctionFS sent an event of unknown type {other}"),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    EndSession,
    StartSession,
    /// Complete the request's status stage.
    Ack,
    Stall,
}

/// What the bridge does for each event, in order. A new session drops the old one first and is up
/// before the request is acked, so the PC's first bytes find it.
pub fn effects(event: &Event) -> &'static [Effect] {
    match event {
        Event::Setup(setup)
            if setup.request_type == VENDOR_OUT_TO_INTERFACE && setup.request == NEW_SESSION && setup.length == 0 =>
        {
            &[Effect::EndSession, Effect::StartSession, Effect::Ack]
        }
        Event::Setup(_) => &[Effect::Stall],
        Event::Disable | Event::Unbind => &[Effect::EndSession],
        Event::Bind | Event::Enable | Event::Suspend | Event::Resume => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn the_descriptors_follow_functionfs_v2() {
        let blob = descriptors();
        assert_eq!(u32_at(&blob, 0), 3, "FUNCTIONFS_DESCRIPTORS_MAGIC_V2");
        assert_eq!(u32_at(&blob, 4) as usize, blob.len());
        assert_eq!(u32_at(&blob, 8), 1 | 2 | 8, "FS, HS and MS OS descriptors");
        assert_eq!([u32_at(&blob, 12), u32_at(&blob, 16), u32_at(&blob, 20)], [3, 3, 2]);

        let fs = &blob[24..24 + 23];
        assert_eq!(fs[..9], [9, 4, 0, 0, 2, 0xFF, 0x47, 0x44, 1]);
        assert_eq!(fs[9..16], [7, 5, 0x01, 2, 64, 0, 0]);
        assert_eq!(fs[16..23], [7, 5, 0x82, 2, 64, 0, 0]);
        let hs = &blob[47..47 + 23];
        assert_eq!(hs[9..16], [7, 5, 0x01, 2, 0x00, 0x02, 0]);
        assert_eq!(hs[16..23], [7, 5, 0x82, 2, 0x00, 0x02, 0]);
    }

    #[test]
    fn the_os_descriptors_ask_for_winusb_and_give_our_guid() {
        let blob = descriptors();
        let compat = &blob[70..70 + 35];
        assert_eq!(compat[0], 0, "interface 0");
        assert_eq!(u32_at(compat, 1), 35, "dwLength counts the header");
        assert_eq!(compat[5..11], [1, 0, 4, 0, 1, 0], "bcdVersion, wIndex 4, bCount 1, Reserved");
        assert_eq!(compat[11..13], [0, 1], "bFirstInterfaceNumber, Reserved1 must be 1");
        assert_eq!(&compat[13..21], b"WINUSB\0\0");
        assert!(compat[21..35].iter().all(|&b| b == 0));

        let prop = &blob[105..];
        let size = u32_at(prop, 11);
        assert_eq!(u32_at(prop, 1), 11 + size);
        assert_eq!(prop[5..11], [1, 0, 5, 0, 1, 0], "bcdVersion, wIndex 5, wCount 1");
        assert_eq!(u32_at(prop, 15), 7, "REG_MULTI_SZ");
        let name_len = u16::from_le_bytes([prop[19], prop[20]]) as usize;
        assert_eq!(name_len, 42);
        let data_len = u32_at(prop, 21 + name_len) as usize;
        assert_eq!(size as usize, 14 + name_len + data_len, "the kernel's length check");
        assert_eq!(prop.len(), 11 + size as usize);
        let data = &prop[25 + name_len..];
        let text: Vec<u16> = data.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&text[..38]).unwrap(), DEVICE_INTERFACE_GUID);
        assert_eq!(text[38..], [0, 0], "one string, then the list's terminator");
    }

    #[test]
    fn the_strings_name_the_interface() {
        let blob = strings();
        assert_eq!([u32_at(&blob, 0), u32_at(&blob, 4), u32_at(&blob, 8), u32_at(&blob, 12)], [2, blob.len() as u32, 1, 1]);
        assert_eq!(blob[16..18], [0x09, 0x04]);
        assert_eq!(&blob[18..], b"Glassdeck\0");
    }

    #[test]
    fn events_decode_from_the_kernels_layout() {
        let mut bytes = [0u8; EVENT_LEN];
        bytes[8] = 2;
        assert_eq!(decode_event(&bytes).unwrap(), Event::Enable);
        let setup = [0x41, 0x01, 0x34, 0x12, 0x00, 0x00, 0x00, 0x00, 4, 0, 0, 0];
        assert_eq!(
            decode_event(&setup).unwrap(),
            Event::Setup(Setup { request_type: 0x41, request: 1, value: 0x1234, index: 0, length: 0 })
        );
        bytes[8] = 9;
        assert!(decode_event(&bytes).is_err());
    }

    #[test]
    fn only_a_new_session_request_starts_a_session() {
        let new_session = Setup { request_type: 0x41, request: NEW_SESSION, value: 0, index: 0, length: 0 };
        assert_eq!(effects(&Event::Setup(new_session)), [Effect::EndSession, Effect::StartSession, Effect::Ack]);
        assert_eq!(effects(&Event::Setup(Setup { request: 2, ..new_session })), [Effect::Stall]);
        assert_eq!(effects(&Event::Setup(Setup { length: 4, ..new_session })), [Effect::Stall]);
        assert_eq!(effects(&Event::Setup(Setup { request_type: 0xC1, ..new_session })), [Effect::Stall]);
        assert_eq!(effects(&Event::Disable), [Effect::EndSession]);
        assert_eq!(effects(&Event::Unbind), [Effect::EndSession]);
        for quiet in [Event::Bind, Event::Enable, Event::Suspend, Event::Resume] {
            assert!(effects(&quiet).is_empty());
        }
    }
}
