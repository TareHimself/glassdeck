//! The receiver's own USB function, added beside the stock bridge's while the receiver runs, which
//! carries the Link over USB. The PC finds it by class, subclass and protocol and starts each session
//! with a vendor request; both ends follow a write that ends on a packet boundary with a zero-length
//! packet, so the other end's read completes.

/// Vendor-specific; subclass 'G' and protocol 'D' tell it from the stock bridge's FF/00/00.
pub const INTERFACE_CLASS: u8 = 0xFF;
pub const INTERFACE_SUBCLASS: u8 = 0x47;
pub const INTERFACE_PROTOCOL: u8 = 0x44;

/// Vendor request to the interface, with no data: end any session and start a new one.
/// Not 0x01: that's the gadget's Microsoft OS descriptor vendor code, which the kernel answers itself.
pub const NEW_SESSION: u8 = 0x47;

/// Windows picks WinUSB for the interface from its Microsoft OS descriptors, and programs open it by
/// this GUID.
pub const DEVICE_INTERFACE_GUID: &str = "{8184EF6B-F0FE-4909-84EA-AAFAC38A073E}";

pub const FULL_SPEED_PACKET: u16 = 64;
pub const HIGH_SPEED_PACKET: u16 = 512;

/// Whether a bulk write of `len` bytes must be followed by a zero-length packet: one that ends exactly
/// on a packet boundary leaves the reader waiting for more otherwise. A multiple of the high-speed
/// packet is a multiple of the full-speed one too.
pub fn needs_zero_length_packet(len: usize) -> bool {
    len > 0 && len.is_multiple_of(HIGH_SPEED_PACKET as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_writes_ending_on_a_packet_boundary_need_a_zero_length_packet() {
        assert!(!needs_zero_length_packet(0));
        assert!(!needs_zero_length_packet(511));
        assert!(needs_zero_length_packet(512));
        assert!(!needs_zero_length_packet(513));
        assert!(needs_zero_length_packet(16 * 1024));
    }
}
