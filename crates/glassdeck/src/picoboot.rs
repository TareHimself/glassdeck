//! The RP2040 boot ROM's PICOBOOT interface (what picotool uses): read flash and reboot. Its
//! drive can only take UF2 files, so this is the only way to read the flash from outside.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, Direction, In, Out};

pub const BOOT_ROM_VID: u16 = 0x2E8A;
pub const BOOT_ROM_PID: u16 = 0x0003;
pub const FLASH_BASE: u32 = 0x1000_0000;
pub const FLASH_SIZE: u32 = 16 * 1024 * 1024;
const PICOBOOT_INTERFACE: u8 = 1;
const MAGIC: u32 = 0x431F_D10B;
const EXCLUSIVE_ACCESS: u8 = 0x01;
const REBOOT: u8 = 0x02;
const READ: u8 = 0x84;
const EXIT_XIP: u8 = 0x06;
/// Locks the drive against writes while we read.
const EXCLUSIVE: u8 = 1;
const NOT_EXCLUSIVE: u8 = 0;
/// Flash reads must start on a 256-byte page.
const READ_CHUNK: u32 = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);
const SRAM_END: u32 = 0x2004_2000;
const REBOOT_DELAY_MS: u32 = 100;

pub struct BootRom {
    out: nusb::Endpoint<Bulk, Out>,
    input: nusb::Endpoint<Bulk, In>,
    token: u32,
}

/// RP2040s in their boot ROM on USB.
pub fn count() -> usize {
    nusb::list_devices().wait().map_or(0, |d| {
        d.filter(|d| d.vendor_id() == BOOT_ROM_VID && d.product_id() == BOOT_ROM_PID)
            .count()
    })
}

impl BootRom {
    pub fn open() -> Result<Self> {
        let info = nusb::list_devices()
            .wait()
            .context("listing USB devices")?
            .find(|d| d.vendor_id() == BOOT_ROM_VID && d.product_id() == BOOT_ROM_PID)
            .context("the RP2040 boot ROM isn't on USB")?;
        let device = info.open().wait().context("opening the RP2040 boot ROM")?;
        let interface = device.claim_interface(PICOBOOT_INTERFACE).wait().context(
            "claiming the boot ROM's PICOBOOT interface. On Windows it has no driver until you give it one: \
             run Zadig, pick \"RP2 Boot (Interface 1)\" and install WinUSB, then try again",
        )?;
        let descriptor = interface
            .descriptor()
            .context("the PICOBOOT interface has no descriptor")?;
        let bulk = |dir: Direction| {
            descriptor
                .endpoints()
                .find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == dir)
                .map(|e| e.address())
                .context("the PICOBOOT interface lacks a bulk endpoint")
        };
        let (out_addr, in_addr) = (bulk(Direction::Out)?, bulk(Direction::In)?);
        Ok(Self {
            out: interface.endpoint::<Bulk, Out>(out_addr)?,
            input: interface.endpoint::<Bulk, In>(in_addr)?,
            token: 1,
        })
    }

    fn command(&mut self, id: u8, args: &[u8], transfer_len: u32) -> Result<()> {
        let mut cmd = [0u8; 32];
        cmd[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        cmd[4..8].copy_from_slice(&self.token.to_le_bytes());
        cmd[8] = id;
        cmd[9] = args.len() as u8;
        cmd[12..16].copy_from_slice(&transfer_len.to_le_bytes());
        cmd[16..16 + args.len()].copy_from_slice(args);
        self.token = self.token.wrapping_add(1);
        let mut buf = Buffer::new(cmd.len());
        buf.extend_from_slice(&cmd);
        self.out
            .transfer_blocking(buf, TIMEOUT)
            .into_result()
            .context("sending a PICOBOOT command")?;
        Ok(())
    }

    /// A command with no data phase; the boot ROM acknowledges with a zero-length IN packet.
    fn simple(&mut self, id: u8, args: &[u8]) -> Result<()> {
        self.command(id, args, 0)?;
        self.input
            .transfer_blocking(Buffer::new(64), TIMEOUT)
            .into_result()
            .with_context(|| format!("PICOBOOT command {id:#04x} wasn't acknowledged"))?;
        Ok(())
    }

    fn exclusive(&mut self, mode: u8) -> Result<()> {
        self.simple(EXCLUSIVE_ACCESS, &[mode])
    }

    /// Reads `len` bytes of flash from `offset`, page-aligned.
    pub fn read_flash(
        &mut self,
        offset: u32,
        len: u32,
        mut progress: impl FnMut(u32),
    ) -> Result<Vec<u8>> {
        if !offset.is_multiple_of(256) || !len.is_multiple_of(256) || offset + len > FLASH_SIZE {
            bail!("a flash read must be whole 256-byte pages inside the 16 MiB chip");
        }
        self.exclusive(EXCLUSIVE)?;
        self.simple(EXIT_XIP, &[])?;
        let mut data = Vec::with_capacity(len as usize);
        let mut at = offset;
        while at < offset + len {
            let n = READ_CHUNK.min(offset + len - at);
            let mut args = [0u8; 8];
            args[..4].copy_from_slice(&(FLASH_BASE + at).to_le_bytes());
            args[4..].copy_from_slice(&n.to_le_bytes());
            self.command(READ, &args, n)?;
            let chunk = self
                .input
                .transfer_blocking(Buffer::new(n as usize), TIMEOUT)
                .into_result()
                .with_context(|| format!("reading flash at {at:#08x}"))?;
            if chunk.len() != n as usize {
                bail!(
                    "the boot ROM sent {} of {n} bytes at {at:#08x}",
                    chunk.len()
                );
            }
            data.extend_from_slice(&chunk);
            self.out
                .transfer_blocking(Buffer::new(0), TIMEOUT)
                .into_result()
                .context("acknowledging a flash read")?;
            at += n;
            progress(at - offset);
        }
        self.exclusive(NOT_EXCLUSIVE)?;
        Ok(data)
    }

    /// Reboots into the firmware in flash.
    pub fn reboot(mut self) -> Result<()> {
        let mut args = [0u8; 12];
        args[4..8].copy_from_slice(&SRAM_END.to_le_bytes());
        args[8..].copy_from_slice(&REBOOT_DELAY_MS.to_le_bytes());
        self.simple(REBOOT, &args)
    }
}
