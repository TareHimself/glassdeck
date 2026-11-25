//! The slice of ZIP a deck uses: stored entries only, data aligned to 4 KB, no ZIP64.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use anyhow::{Context, Result, bail, ensure};

pub const ALIGNMENT: u64 = 4096;

const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const END_SIGNATURE: u32 = 0x0605_4b50;
const LOCAL_HEADER_LEN: u64 = 30;
const CENTRAL_HEADER_LEN: usize = 46;
const END_RECORD_LEN: usize = 22;
const MAX_COMMENT_LEN: usize = u16::MAX as usize;
const STORED: u16 = 0;
const FLAG_ENCRYPTED: u16 = 1;
const FLAG_UTF8_NAME: u16 = 1 << 11;
const VERSION_NEEDED: u16 = 20;
/// Android zipalign's extra field: a u16 alignment, then padding.
const ALIGNMENT_EXTRA_ID: u16 = 0xD935;
const ALIGNMENT_EXTRA_MIN_LEN: u64 = 6;
/// 1980-01-01 00:00 in DOS time: entries carry no meaningful timestamps.
const DOS_DATE_1980: u16 = (1 << 5) | 1;

/// An entry's data as a byte range of the deck file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub offset: u64,
    pub len: u64,
}

/// Reads the central directory once and finds where each entry's data starts.
pub fn read_directory(file: &mut File) -> Result<HashMap<String, Entry>> {
    let file_len = file.seek(SeekFrom::End(0))?;
    let tail_len = file_len.min((END_RECORD_LEN + MAX_COMMENT_LEN) as u64);
    let mut tail = vec![0; tail_len as usize];
    file.seek(SeekFrom::Start(file_len - tail_len))?;
    file.read_exact(&mut tail)?;
    let end = (0..=tail.len().saturating_sub(END_RECORD_LEN))
        .rev()
        .find(|&i| u32_at(&tail, i) == END_SIGNATURE)
        .context("not a zip file (no end of central directory)")?;
    let count = u16_at(&tail, end + 10) as usize;
    let directory_len = u32_at(&tail, end + 12) as u64;
    let directory_offset = u32_at(&tail, end + 16) as u64;
    ensure!(directory_offset + directory_len <= file_len, "the central directory runs past the end of the file");

    let mut directory = vec![0; directory_len as usize];
    file.seek(SeekFrom::Start(directory_offset))?;
    file.read_exact(&mut directory)?;

    let mut entries = HashMap::with_capacity(count);
    let mut at = 0;
    for _ in 0..count {
        ensure!(at + CENTRAL_HEADER_LEN <= directory.len() && u32_at(&directory, at) == CENTRAL_SIGNATURE, "corrupt central directory");
        let flags = u16_at(&directory, at + 8);
        let method = u16_at(&directory, at + 10);
        let compressed = u32_at(&directory, at + 20) as u64;
        let size = u32_at(&directory, at + 24) as u64;
        let name_len = u16_at(&directory, at + 28) as usize;
        let extra_len = u16_at(&directory, at + 30) as usize;
        let comment_len = u16_at(&directory, at + 32) as usize;
        let local_offset = u32_at(&directory, at + 42) as u64;
        let name_at = at + CENTRAL_HEADER_LEN;
        ensure!(name_at + name_len <= directory.len(), "corrupt central directory");
        let name = String::from_utf8(directory[name_at..name_at + name_len].to_vec()).context("an entry name is not UTF-8")?;
        at = name_at + name_len + extra_len + comment_len;

        if method != STORED {
            bail!("entry {name:?} is compressed (method {method}); a deck holds stored entries only");
        }
        ensure!(flags & FLAG_ENCRYPTED == 0, "entry {name:?} is encrypted");
        ensure!(compressed == size, "entry {name:?} has different stored and real sizes");
        if name.ends_with('/') {
            continue;
        }
        let offset = data_offset(file, local_offset, &name)?;
        ensure!(offset + size <= file_len, "entry {name:?} runs past the end of the file");
        if entries.insert(name.clone(), Entry { offset, len: size }).is_some() {
            bail!("entry {name:?} appears twice");
        }
    }
    Ok(entries)
}

fn data_offset(file: &mut File, local_offset: u64, name: &str) -> Result<u64> {
    let mut header = [0; LOCAL_HEADER_LEN as usize];
    file.seek(SeekFrom::Start(local_offset))?;
    file.read_exact(&mut header).with_context(|| format!("reading the local header of {name:?}"))?;
    ensure!(u32_at(&header, 0) == LOCAL_SIGNATURE, "entry {name:?} has no local header");
    let name_len = u16_at(&header, 26) as u64;
    let extra_len = u16_at(&header, 28) as u64;
    Ok(local_offset + LOCAL_HEADER_LEN + name_len + extra_len)
}

/// Writes stored, aligned entries in order, then the central directory.
pub struct ZipWriter {
    file: File,
    written: Vec<Written>,
}

struct Written {
    name: String,
    crc: u32,
    len: u32,
    local_offset: u32,
}

impl ZipWriter {
    pub fn new(file: File) -> Self {
        Self { file, written: Vec::new() }
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.written.iter().map(|w| w.name.as_str())
    }

    /// Copies `data` into a new entry, computing its CRC on the way, then fills in the header.
    pub fn add(&mut self, name: &str, data: &mut dyn Read) -> Result<()> {
        check_name(name)?;
        ensure!(!self.written.iter().any(|w| w.name == name), "entry {name:?} added twice");
        let local_offset = self.file.stream_position()?;
        let extra_len = alignment_extra_len(local_offset, name.len() as u64);
        self.file.write_all(&local_header(name, extra_len, 0, 0))?;
        let mut extra = vec![0u8; extra_len as usize];
        extra[0..2].copy_from_slice(&ALIGNMENT_EXTRA_ID.to_le_bytes());
        extra[2..4].copy_from_slice(&((extra_len - 4) as u16).to_le_bytes());
        extra[4..6].copy_from_slice(&(ALIGNMENT as u16).to_le_bytes());
        self.file.write_all(&extra)?;

        let mut crc = Crc32::new();
        let mut len = 0u64;
        let mut buffer = vec![0; 1 << 20];
        loop {
            let n = data.read(&mut buffer).with_context(|| format!("reading the data for {name:?}"))?;
            if n == 0 {
                break;
            }
            crc.update(&buffer[..n]);
            self.file.write_all(&buffer[..n])?;
            len += n as u64;
        }
        let len = u32::try_from(len).with_context(|| format!("entry {name:?} is over 4 GB"))?;
        let local_offset = u32::try_from(local_offset).context("the deck is over 4 GB")?;
        let end = self.file.stream_position()?;
        self.file.seek(SeekFrom::Start(local_offset as u64))?;
        self.file.write_all(&local_header(name, extra_len, crc.value(), len))?;
        self.file.seek(SeekFrom::Start(end))?;
        self.written.push(Written { name: name.to_owned(), crc: crc.value(), len, local_offset });
        Ok(())
    }

    pub fn finish(mut self) -> Result<File> {
        let directory_offset = self.file.stream_position()?;
        let mut directory = Vec::new();
        for w in &self.written {
            directory.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
            directory.extend_from_slice(&VERSION_NEEDED.to_le_bytes());
            directory.extend_from_slice(&VERSION_NEEDED.to_le_bytes());
            directory.extend_from_slice(&FLAG_UTF8_NAME.to_le_bytes());
            directory.extend_from_slice(&STORED.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&DOS_DATE_1980.to_le_bytes());
            directory.extend_from_slice(&w.crc.to_le_bytes());
            directory.extend_from_slice(&w.len.to_le_bytes());
            directory.extend_from_slice(&w.len.to_le_bytes());
            directory.extend_from_slice(&(w.name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&[0; 8]);
            directory.extend_from_slice(&0u32.to_le_bytes());
            directory.extend_from_slice(&w.local_offset.to_le_bytes());
            directory.extend_from_slice(w.name.as_bytes());
        }
        let count = u16::try_from(self.written.len()).context("a deck holds at most 65535 entries")?;
        let mut end = Vec::with_capacity(END_RECORD_LEN);
        end.extend_from_slice(&END_SIGNATURE.to_le_bytes());
        end.extend_from_slice(&[0; 4]);
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        end.extend_from_slice(&u32::try_from(directory_offset).context("the deck is over 4 GB")?.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        self.file.write_all(&directory)?;
        self.file.write_all(&end)?;
        self.file.sync_all()?;
        Ok(self.file)
    }
}

/// Extra-field length that puts the data of an entry at `local_offset` on a 4 KB boundary.
fn alignment_extra_len(local_offset: u64, name_len: u64) -> u64 {
    let unpadded = local_offset + LOCAL_HEADER_LEN + name_len + ALIGNMENT_EXTRA_MIN_LEN;
    ALIGNMENT_EXTRA_MIN_LEN + (ALIGNMENT - unpadded % ALIGNMENT) % ALIGNMENT
}

fn local_header(name: &str, extra_len: u64, crc: u32, len: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(LOCAL_HEADER_LEN as usize + name.len());
    h.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
    h.extend_from_slice(&VERSION_NEEDED.to_le_bytes());
    h.extend_from_slice(&FLAG_UTF8_NAME.to_le_bytes());
    h.extend_from_slice(&STORED.to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes());
    h.extend_from_slice(&DOS_DATE_1980.to_le_bytes());
    h.extend_from_slice(&crc.to_le_bytes());
    h.extend_from_slice(&len.to_le_bytes());
    h.extend_from_slice(&len.to_le_bytes());
    h.extend_from_slice(&(name.len() as u16).to_le_bytes());
    h.extend_from_slice(&(extra_len as u16).to_le_bytes());
    h.extend_from_slice(name.as_bytes());
    h
}

/// Entry names are relative, `/`-separated paths with no `.` or `..` parts.
pub fn check_name(name: &str) -> Result<()> {
    ensure!(!name.is_empty() && name.len() <= u16::MAX as usize, "bad entry name {name:?}");
    ensure!(!name.starts_with('/') && !name.contains('\\'), "entry name {name:?} must be relative and use /");
    ensure!(
        name.split('/').all(|part| !part.is_empty() && part != "." && part != ".."),
        "entry name {name:?} has an empty, . or .. part"
    );
    Ok(())
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

struct Crc32(u32);

impl Crc32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };

    fn new() -> Self {
        Self(!0)
    }

    fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.0 = Self::TABLE[((self.0 ^ b as u32) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }

    fn value(&self) -> u32 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_standard_check_value() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.value(), 0xCBF4_3926);
    }

    #[test]
    fn padding_lands_data_on_4k() {
        for (offset, name_len) in [(0, 13), (4096, 1), (5000, 200), (8191, 0)] {
            let extra = alignment_extra_len(offset, name_len);
            assert!(extra >= ALIGNMENT_EXTRA_MIN_LEN);
            assert_eq!((offset + LOCAL_HEADER_LEN + name_len + extra) % ALIGNMENT, 0);
        }
    }

    #[test]
    fn names_must_be_plain_relative_paths() {
        assert!(check_name("web/js/app.js").is_ok());
        for bad in ["", "/abs", "a\\b", "a/../b", "./a", "a//b"] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }
}
