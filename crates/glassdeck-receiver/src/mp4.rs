//! Just enough MP4 to play a deck's video: one H.264 or HEVC track, its sample table, and its
//! parameter sets, with samples turned into Annex-B access units for the decoder.

use anyhow::{Context, Result, bail, ensure};
use glassdeck_proto::Codec;

const BOX_HEADER: usize = 8;
const LARGE_BOX_HEADER: usize = 16;
/// Moov for a long 1080p wallpaper is a few hundred KB; anything far larger isn't a deck video.
const MAX_MOOV_LEN: u64 = 64 << 20;
const VISUAL_SAMPLE_ENTRY_LEN: usize = 78;
const ANNEX_B_START: [u8; 4] = [0, 0, 0, 1];

/// Reads bytes at an offset; a deck entry or a plain file.
pub trait ReadAt {
    fn len(&self) -> u64;
    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<()>;
}

impl ReadAt for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<()> {
        let start = usize::try_from(at)?;
        let end = start.checked_add(buf.len()).context("read past the end")?;
        buf.copy_from_slice(self.get(start..end).context("read past the end")?);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    /// Presentation time from the first sample's decode time, in microseconds
    pub pts_us: u64,
    pub keyframe: bool,
}

#[derive(Debug)]
pub struct VideoTrack {
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
    /// Decode order
    pub samples: Vec<Sample>,
    /// One loop's length, so a looping player can keep presentation times rising
    pub duration_us: u64,
    nal_length_size: usize,
    /// Annex-B parameter sets, sent ahead of every keyframe
    parameter_sets: Vec<u8>,
}

impl VideoTrack {
    pub fn read(source: &(impl ReadAt + ?Sized)) -> Result<Self> {
        let moov = find_moov(source)?;
        let video = boxes(&moov)?
            .filter(|b| b.kind == *b"trak")
            .find_map(|trak| match is_video(trak.body) {
                Ok(true) => Some(Ok(trak.body)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .context("the MP4 has no video track")??;
        parse_track(video)
    }

    /// The sample as one Annex-B access unit, with parameter sets in front of keyframes.
    pub fn access_unit(&self, sample: Sample, data: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(data.len() + self.parameter_sets.len() + 16);
        if sample.keyframe {
            out.extend_from_slice(&self.parameter_sets);
        }
        let mut rest = data;
        while !rest.is_empty() {
            ensure!(rest.len() >= self.nal_length_size, "a sample ends inside a NAL length");
            let (len, after) = rest.split_at(self.nal_length_size);
            let len = len.iter().fold(0usize, |n, &b| n << 8 | b as usize);
            ensure!(after.len() >= len, "a NAL runs past its sample");
            out.extend_from_slice(&ANNEX_B_START);
            out.extend_from_slice(&after[..len]);
            rest = &after[len..];
        }
        Ok(out)
    }
}

struct Mp4Box<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

fn boxes(data: &[u8]) -> Result<impl Iterator<Item = Mp4Box<'_>>> {
    let mut found = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let (kind, header, len) = box_header(rest)?;
        let len = usize::try_from(len)?;
        ensure!(len >= header && len <= rest.len(), "box {} has a bad length", String::from_utf8_lossy(&kind));
        found.push(Mp4Box { kind, body: &rest[header..len] });
        rest = &rest[len..];
    }
    Ok(found.into_iter())
}

/// (kind, header length, box length); a length of 0 means "to the end" and is only valid at the top level.
fn box_header(data: &[u8]) -> Result<([u8; 4], usize, u64)> {
    ensure!(data.len() >= BOX_HEADER, "truncated box header");
    let kind: [u8; 4] = data[4..8].try_into().expect("4 bytes");
    match be_u32(data, 0)? {
        1 => {
            ensure!(data.len() >= LARGE_BOX_HEADER, "truncated large box header");
            Ok((kind, LARGE_BOX_HEADER, be_u64(data, 8)?))
        }
        len => Ok((kind, BOX_HEADER, len as u64)),
    }
}

fn find_moov(source: &(impl ReadAt + ?Sized)) -> Result<Vec<u8>> {
    let mut at = 0;
    while at < source.len() {
        let mut header = [0u8; LARGE_BOX_HEADER];
        let available = (source.len() - at).min(LARGE_BOX_HEADER as u64) as usize;
        ensure!(available >= BOX_HEADER, "the MP4 ends inside a box header");
        source.read_at(at, &mut header[..available])?;
        let (kind, header_len, len) = box_header(&header[..available])?;
        let len = if len == 0 { source.len() - at } else { len };
        ensure!(len >= header_len as u64 && at + len <= source.len(), "top-level box has a bad length");
        if kind == *b"moov" {
            ensure!(len <= MAX_MOOV_LEN, "moov of {len} bytes is too large");
            let mut moov = vec![0; (len - header_len as u64) as usize];
            source.read_at(at + header_len as u64, &mut moov)?;
            return Ok(moov);
        }
        at += len;
    }
    bail!("the MP4 has no moov box")
}

fn child<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> Result<&'a [u8]> {
    let mut current = data;
    for kind in path {
        current = boxes(current)?
            .find(|b| b.kind == **kind)
            .map(|b| b.body)
            .with_context(|| format!("missing {} box", String::from_utf8_lossy(*kind)))?;
    }
    Ok(current)
}

fn is_video(trak: &[u8]) -> Result<bool> {
    let hdlr = child(trak, &[b"mdia", b"hdlr"])?;
    Ok(hdlr.get(8..12) == Some(b"vide"))
}

fn parse_track(trak: &[u8]) -> Result<VideoTrack> {
    let mdia = child(trak, &[b"mdia"])?;
    let timescale = timescale(child(mdia, &[b"mdhd"])?)?;
    let stbl = child(mdia, &[b"minf", b"stbl"])?;
    let entry = SampleEntry::parse(child(stbl, &[b"stsd"])?)?;

    let sizes = sample_sizes(child(stbl, &[b"stsz"])?)?;
    let deltas = expand_runs(child(stbl, &[b"stts"])?, sizes.len())?;
    let composition = match child(stbl, &[b"ctts"]) {
        Ok(ctts) => composition_offsets(ctts, sizes.len())?,
        Err(_) => vec![0; sizes.len()],
    };
    let offsets = sample_offsets(stbl, &sizes)?;
    let sync = match child(stbl, &[b"stss"]) {
        Ok(stss) => Some(sync_samples(stss)?),
        Err(_) => None,
    };

    let to_us = |ticks: i128| (ticks * 1_000_000 / timescale as i128).max(0) as u64;
    let mut decode_time: i128 = 0;
    let mut presented = Vec::with_capacity(sizes.len());
    for i in 0..sizes.len() {
        presented.push((decode_time + composition[i] as i128, deltas[i] as i128));
        decode_time += deltas[i] as i128;
    }
    let first = presented.iter().map(|&(pts, _)| pts).min().context("the video track has no samples")?;
    let end = presented.iter().map(|&(pts, delta)| pts + delta).max().expect("not empty");
    let samples: Vec<Sample> = presented
        .iter()
        .enumerate()
        .map(|(i, &(pts, _))| Sample {
            offset: offsets[i],
            size: sizes[i],
            pts_us: to_us(pts - first),
            keyframe: sync.as_ref().is_none_or(|sync| sync.binary_search(&(i as u32 + 1)).is_ok()),
        })
        .collect();
    ensure!(samples[0].keyframe, "the video track does not start on a keyframe");

    Ok(VideoTrack {
        codec: entry.codec,
        width: entry.width,
        height: entry.height,
        samples,
        duration_us: to_us(end - first),
        nal_length_size: entry.nal_length_size,
        parameter_sets: entry.parameter_sets,
    })
}

fn timescale(mdhd: &[u8]) -> Result<u32> {
    let at = match mdhd.first() {
        Some(0) => 12,
        Some(1) => 20,
        _ => bail!("unknown mdhd version"),
    };
    let timescale = be_u32(mdhd, at)?;
    ensure!(timescale > 0, "the video track has a zero timescale");
    Ok(timescale)
}

struct SampleEntry {
    codec: Codec,
    width: u16,
    height: u16,
    nal_length_size: usize,
    parameter_sets: Vec<u8>,
}

impl SampleEntry {
    fn parse(stsd: &[u8]) -> Result<Self> {
        ensure!(be_u32(stsd, 4)? >= 1, "stsd lists no sample entries");
        let entry = boxes(stsd.get(8..).context("truncated stsd")?)?.next().context("stsd lists no sample entries")?;
        let fields = entry.body.get(..VISUAL_SAMPLE_ENTRY_LEN).context("truncated visual sample entry")?;
        let (width, height) = (be_u16(fields, 24)?, be_u16(fields, 26)?);
        let children = &entry.body[VISUAL_SAMPLE_ENTRY_LEN..];
        let (codec, config) = match &entry.kind {
            b"avc1" | b"avc3" => (Codec::H264, child(children, &[b"avcC"])?),
            b"hvc1" | b"hev1" => (Codec::Hevc, child(children, &[b"hvcC"])?),
            other => bail!("video is {}, not H.264 or HEVC", String::from_utf8_lossy(other)),
        };
        let (nal_length_size, parameter_sets) = match codec {
            Codec::H264 => avc_parameter_sets(config)?,
            Codec::Hevc => hevc_parameter_sets(config)?,
        };
        Ok(Self { codec, width, height, nal_length_size, parameter_sets })
    }
}

fn avc_parameter_sets(avcc: &[u8]) -> Result<(usize, Vec<u8>)> {
    let nal_length_size = (*avcc.get(4).context("truncated avcC")? & 3) as usize + 1;
    let mut out = Vec::new();
    let mut at = 5;
    for count_mask in [0x1f, 0xff] {
        let count = *avcc.get(at).context("truncated avcC")? & count_mask;
        at += 1;
        for _ in 0..count {
            at = copy_nal(avcc, at, &mut out)?;
        }
    }
    Ok((nal_length_size, out))
}

fn hevc_parameter_sets(hvcc: &[u8]) -> Result<(usize, Vec<u8>)> {
    let nal_length_size = (*hvcc.get(21).context("truncated hvcC")? & 3) as usize + 1;
    let arrays = *hvcc.get(22).context("truncated hvcC")?;
    let mut out = Vec::new();
    let mut at = 23;
    for _ in 0..arrays {
        let count = be_u16(hvcc, at + 1)?;
        at += 3;
        for _ in 0..count {
            at = copy_nal(hvcc, at, &mut out)?;
        }
    }
    Ok((nal_length_size, out))
}

fn copy_nal(data: &[u8], at: usize, out: &mut Vec<u8>) -> Result<usize> {
    let len = be_u16(data, at)? as usize;
    let nal = data.get(at + 2..at + 2 + len).context("truncated parameter set")?;
    out.extend_from_slice(&ANNEX_B_START);
    out.extend_from_slice(nal);
    Ok(at + 2 + len)
}

fn sample_sizes(stsz: &[u8]) -> Result<Vec<u32>> {
    let fixed = be_u32(stsz, 4)?;
    let count = be_u32(stsz, 8)? as usize;
    match fixed {
        0 => (0..count).map(|i| be_u32(stsz, 12 + 4 * i)).collect(),
        size => Ok(vec![size; count]),
    }
}

/// stts as one delta per sample.
fn expand_runs(stts: &[u8], samples: usize) -> Result<Vec<u32>> {
    let mut out = Vec::with_capacity(samples);
    for run in 0..be_u32(stts, 4)? as usize {
        let (count, delta) = (be_u32(stts, 8 + 8 * run)?, be_u32(stts, 12 + 8 * run)?);
        out.extend(std::iter::repeat_n(delta, count as usize));
    }
    ensure!(out.len() == samples, "stts covers {} samples, stsz {samples}", out.len());
    Ok(out)
}

fn composition_offsets(ctts: &[u8], samples: usize) -> Result<Vec<i64>> {
    let signed = ctts.first() == Some(&1);
    let mut out = Vec::with_capacity(samples);
    for run in 0..be_u32(ctts, 4)? as usize {
        let count = be_u32(ctts, 8 + 8 * run)?;
        let raw = be_u32(ctts, 12 + 8 * run)?;
        let offset = if signed { raw as i32 as i64 } else { raw as i64 };
        out.extend(std::iter::repeat_n(offset, count as usize));
    }
    ensure!(out.len() == samples, "ctts covers {} samples, stsz {samples}", out.len());
    Ok(out)
}

fn sample_offsets(stbl: &[u8], sizes: &[u32]) -> Result<Vec<u64>> {
    let chunks: Vec<u64> = match (child(stbl, &[b"stco"]), child(stbl, &[b"co64"])) {
        (Ok(stco), _) => {
            (0..be_u32(stco, 4)? as usize).map(|i| be_u32(stco, 8 + 4 * i).map(u64::from)).collect::<Result<_>>()?
        }
        (_, Ok(co64)) => (0..be_u32(co64, 4)? as usize).map(|i| be_u64(co64, 8 + 8 * i)).collect::<Result<_>>()?,
        _ => bail!("the video track has no chunk offsets"),
    };
    let stsc = child(stbl, &[b"stsc"])?;
    let runs: Vec<(u32, u32)> = (0..be_u32(stsc, 4)? as usize)
        .map(|i| Ok((be_u32(stsc, 8 + 12 * i)?, be_u32(stsc, 12 + 12 * i)?)))
        .collect::<Result<_>>()?;

    let mut out = Vec::with_capacity(sizes.len());
    let mut sample = 0;
    for (chunk_index, &chunk_offset) in chunks.iter().enumerate() {
        let chunk_number = chunk_index as u32 + 1;
        let per_chunk =
            runs.iter().rev().find(|(first, _)| *first <= chunk_number).context("stsc does not cover chunk 1")?.1;
        let mut at = chunk_offset;
        for _ in 0..per_chunk {
            let size = *sizes.get(sample).context("stsc lists more samples than stsz")?;
            out.push(at);
            at += size as u64;
            sample += 1;
        }
    }
    ensure!(sample == sizes.len(), "chunks hold {sample} samples, stsz {}", sizes.len());
    Ok(out)
}

fn sync_samples(stss: &[u8]) -> Result<Vec<u32>> {
    let mut out: Vec<u32> = (0..be_u32(stss, 4)? as usize).map(|i| be_u32(stss, 8 + 4 * i)).collect::<Result<_>>()?;
    out.sort_unstable();
    Ok(out)
}

fn be_u16(data: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(data.get(at..at + 2).context("truncated box")?.try_into().expect("2 bytes")))
}

fn be_u32(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(data.get(at..at + 4).context("truncated box")?.try_into().expect("4 bytes")))
}

fn be_u64(data: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(data.get(at..at + 8).context("truncated box")?.try_into().expect("8 bytes")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat()
    }

    fn full_box(kind: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        mp4_box(kind, &[&[version, 0, 0, 0][..], body].concat())
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    const SPS: &[u8] = &[0x67, 0x64, 0x00, 0x28];
    const PPS: &[u8] = &[0x68, 0xee, 0x3c, 0x80];

    /// An H.264 track of 4 samples at 90 kHz, 30 fps, IPBB order with a ctts, in two chunks.
    fn sample_mp4(mdat_at: u32) -> Vec<u8> {
        let avcc = [
            &[1, 0x64, 0, 0x28, 0xff, 0xe1][..],
            &(SPS.len() as u16).to_be_bytes(),
            SPS,
            &[1],
            &(PPS.len() as u16).to_be_bytes(),
            PPS,
        ]
        .concat();
        let mut visual = vec![0u8; VISUAL_SAMPLE_ENTRY_LEN];
        visual[24..26].copy_from_slice(&1920u16.to_be_bytes());
        visual[26..28].copy_from_slice(&1080u16.to_be_bytes());
        let avc1 = mp4_box(b"avc1", &[visual, mp4_box(b"avcC", &avcc)].concat());
        let stbl = mp4_box(
            b"stbl",
            &[
                full_box(b"stsd", 0, &[words(&[1]), avc1].concat()),
                full_box(b"stts", 0, &words(&[1, 4, 3000])),
                full_box(b"ctts", 0, &words(&[3, 1, 3000, 1, 9000, 2, 0])),
                full_box(b"stsz", 0, &words(&[0, 4, 9, 6, 6, 6])),
                full_box(b"stsc", 0, &words(&[1, 1, 2, 1])),
                full_box(b"stco", 0, &words(&[2, mdat_at, mdat_at + 15])),
                full_box(b"stss", 0, &words(&[1, 1])),
            ]
            .concat(),
        );
        let mdhd = full_box(b"mdhd", 0, &words(&[0, 0, 90_000, 12_000, 0]));
        let hdlr = full_box(b"hdlr", 0, &[words(&[0]), b"vide".to_vec(), vec![0; 13]].concat());
        let mdia = mp4_box(b"mdia", &[mdhd, hdlr, mp4_box(b"minf", &stbl)].concat());
        let moov = mp4_box(b"moov", &mp4_box(b"trak", &mdia));
        [mp4_box(b"ftyp", b"isom"), moov].concat()
    }

    #[test]
    fn reads_the_sample_table() {
        let file = sample_mp4(1000);
        let track = VideoTrack::read(file.as_slice()).unwrap();
        assert_eq!((track.codec, track.width, track.height), (Codec::H264, 1920, 1080));
        let offsets: Vec<_> = track.samples.iter().map(|s| (s.offset, s.size)).collect();
        assert_eq!(offsets, [(1000, 9), (1009, 6), (1015, 6), (1021, 6)]);
        let pts: Vec<_> = track.samples.iter().map(|s| s.pts_us).collect();
        assert_eq!(pts, [0, 100_000, 33_333, 66_666]);
        let keyframes: Vec<_> = track.samples.iter().map(|s| s.keyframe).collect();
        assert_eq!(keyframes, [true, false, false, false]);
        assert_eq!(track.duration_us, 133_333);
    }

    #[test]
    fn finds_moov_after_mdat() {
        let mdat = mp4_box(b"mdat", &[0; 32]);
        let moov_first = sample_mp4(8 + 40);
        let file = [mdat, moov_first].concat();
        assert!(VideoTrack::read(file.as_slice()).is_ok());
    }

    #[test]
    fn keyframes_become_annex_b_with_parameter_sets() {
        let file = sample_mp4(0);
        let track = VideoTrack::read(file.as_slice()).unwrap();
        let sample = [&[0, 0, 0, 2][..], &[0x65, 0xaa], &[0, 0, 0, 1], &[0x06]].concat();
        let expected =
            [&ANNEX_B_START[..], SPS, &ANNEX_B_START, PPS, &ANNEX_B_START, &[0x65, 0xaa], &ANNEX_B_START, &[0x06]]
                .concat();
        assert_eq!(track.access_unit(track.samples[0], &sample).unwrap(), expected);
        let delta = track.access_unit(track.samples[1], &[0, 0, 0, 1, 0x41]).unwrap();
        assert_eq!(delta, [0, 0, 0, 1, 0x41]);
    }

    #[test]
    fn refuses_a_nal_that_runs_past_its_sample() {
        let file = sample_mp4(0);
        let track = VideoTrack::read(file.as_slice()).unwrap();
        assert!(track.access_unit(track.samples[1], &[0, 0, 0, 9, 0x41]).is_err());
    }

    #[test]
    fn refuses_files_without_video() {
        assert!(VideoTrack::read(mp4_box(b"ftyp", b"isom").as_slice()).is_err());
        assert!(VideoTrack::read([0u8; 3].as_slice()).is_err());
    }
}
