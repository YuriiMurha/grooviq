//! Bounded, local-only lossless storage for the complete M3 WAV.
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
pub const CHUNK: usize = 8 * 1024 * 1024;
pub const MAX_WAV: usize = super::FRAMES * 8 + 4096;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub name: String,
    pub size: usize,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub frames: usize,
    pub rate: u32,
    pub channels: u16,
    pub format: String,
    pub size: usize,
    pub sha256: String,
    pub chunks: Vec<Chunk>,
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn store(dir: &Path, bytes: &[u8]) {
    assert!(bytes.len() <= MAX_WAV);
    fs::create_dir_all(dir).unwrap();
    let chunks = bytes
        .chunks(CHUNK)
        .enumerate()
        .map(|(i, b)| {
            let name = format!("{i:03}.zlib");
            let mut e = ZlibEncoder::new(Vec::new(), Compression::best());
            e.write_all(b).unwrap();
            let packed = e.finish().unwrap();
            let path = dir.join(&name);
            if fs::read(&path).ok().as_deref() != Some(&packed) {
                fs::write(path, packed).unwrap();
            }
            Chunk {
                name,
                size: b.len(),
                sha256: hash(b),
            }
        })
        .collect();
    let m = Manifest {
        version: 1,
        frames: super::FRAMES,
        rate: 48000,
        channels: 2,
        format: "f32le-wav".into(),
        size: bytes.len(),
        sha256: hash(bytes),
        chunks,
    };
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&m).unwrap(),
    )
    .unwrap();
}
pub fn reconstruct(
    m: &Manifest,
    read: impl FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    reconstruct_bounded(m, super::FRAMES, read)
}
fn reconstruct_bounded(
    m: &Manifest,
    frames: usize,
    mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let bad = || "invalid M3 reference".to_string();
    if m.version != 1
        || m.frames != frames
        || m.rate != 48000
        || m.channels != 2
        || m.format != "f32le-wav"
        || m.size < frames * 8
        || m.size > frames * 8 + 4096
        || m.chunks.len() != m.size.div_ceil(CHUNK)
    {
        return Err(bad());
    }
    let mut total = 0usize;
    for (i, c) in m.chunks.iter().enumerate() {
        if c.name != format!("{i:03}.zlib")
            || c.size != (m.size - total).min(CHUNK)
            || c.sha256.len() != 64
        {
            return Err(bad());
        }
        total = total.checked_add(c.size).ok_or_else(bad)?;
    }
    if total != m.size {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(m.size);
    for c in &m.chunks {
        let packed = read(&c.name)?;
        if packed.len() > CHUNK + 65536 {
            return Err(bad());
        }
        let mut d = ZlibDecoder::new(packed.as_slice());
        let mut b = Vec::new();
        (&mut d)
            .take(c.size as u64 + 1)
            .read_to_end(&mut b)
            .map_err(|e| e.to_string())?;
        if b.len() != c.size || d.total_in() != packed.len() as u64 || hash(&b) != c.sha256 {
            return Err(bad());
        }
        out.extend(b);
    }
    if hash(&out) != m.sha256 {
        return Err(bad());
    }
    validate_wav(&out, frames)?;
    Ok(out)
}
// Integrity and declared geometry do not establish a WAV. Validate the complete
// bounded RIFF container and IEEE-float format fields before accepting the
// reference. Unknown RIFF chunks are skipped with their mandatory odd-byte pad.
fn validate_wav(bytes: &[u8], frames: usize) -> Result<(), String> {
    let bad = || "invalid M3 WAV payload".to_string();
    let data_bytes = frames.checked_mul(8).ok_or_else(bad)?;
    if bytes.len() < 12
        || &bytes[..4] != b"RIFF"
        || &bytes[8..12] != b"WAVE"
        || u64::from(u32::from_le_bytes(bytes[4..8].try_into().unwrap())) + 8 != bytes.len() as u64
    {
        return Err(bad());
    }
    let mut offset = 12usize;
    let mut format_seen = false;
    let mut data_seen = false;
    while offset < bytes.len() {
        let start = offset
            .checked_add(8)
            .filter(|n| *n <= bytes.len())
            .ok_or_else(bad)?;
        let size = u32::from_le_bytes(bytes[offset + 4..start].try_into().unwrap()) as usize;
        let end = start
            .checked_add(size)
            .filter(|n| *n <= bytes.len())
            .ok_or_else(bad)?;
        match &bytes[offset..offset + 4] {
            b"fmt " => {
                if format_seen || data_seen || size < 16 {
                    return Err(bad());
                }
                let f = &bytes[start..end];
                if u16::from_le_bytes(f[2..4].try_into().unwrap()) != 2
                    || u32::from_le_bytes(f[4..8].try_into().unwrap()) != 48000
                    || u32::from_le_bytes(f[8..12].try_into().unwrap()) != 48000 * 8
                    || u16::from_le_bytes(f[12..14].try_into().unwrap()) != 8
                    || u16::from_le_bytes(f[14..16].try_into().unwrap()) != 32
                {
                    return Err(bad());
                }
                match u16::from_le_bytes(f[..2].try_into().unwrap()) {
                    3 if size == 16 || (size == 18 && f[16..18] == [0, 0]) => {}
                    0xfffe if size == 40 => {
                        const FLOAT_GUID: [u8; 16] =
                            [3, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
                        let mask = u32::from_le_bytes(f[20..24].try_into().unwrap());
                        if u16::from_le_bytes(f[16..18].try_into().unwrap()) != 22
                            || u16::from_le_bytes(f[18..20].try_into().unwrap()) != 32
                            || f[24..40] != FLOAT_GUID
                            || (mask != 0 && mask.count_ones() != 2)
                        {
                            return Err(bad());
                        }
                    }
                    _ => return Err(bad()),
                }
                format_seen = true;
            }
            b"data" => {
                if data_seen || !format_seen || size != data_bytes {
                    return Err(bad());
                }
                data_seen = true;
            }
            _ => {}
        }
        offset = end
            .checked_add(size & 1)
            .filter(|n| *n <= bytes.len())
            .ok_or_else(bad)?;
    }
    if !format_seen || !data_seen {
        return Err(bad());
    }
    Ok(())
}

pub fn load(dir: &Path) -> Result<Vec<u8>, String> {
    let f = fs::File::open(dir.join("manifest.json")).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    f.take(16385)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 16384 {
        return Err("manifest too large".into());
    }
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    reconstruct(&m, |name| {
        let f = fs::File::open(dir.join(name)).map_err(|e| e.to_string())?;
        let mut b = Vec::new();
        f.take((CHUNK + 65537) as u64)
            .read_to_end(&mut b)
            .map_err(|e| e.to_string())?;
        Ok(b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packed(b: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::fast());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    }
    fn wave() -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(
            &mut cursor,
            hound::WavSpec {
                channels: 2,
                sample_rate: 48000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for sample in [0.125f32, -0.25, 0.5, -0.5] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        cursor.into_inner()
    }
    fn fixture(bytes: &[u8]) -> (Manifest, Vec<u8>) {
        (
            Manifest {
                version: 1,
                frames: 2,
                rate: 48000,
                channels: 2,
                format: "f32le-wav".into(),
                size: bytes.len(),
                sha256: hash(bytes),
                chunks: vec![Chunk {
                    name: "000.zlib".into(),
                    size: bytes.len(),
                    sha256: hash(bytes),
                }],
            },
            packed(bytes),
        )
    }
    fn tiny() -> (Manifest, Vec<u8>) {
        fixture(&wave())
    }
    fn classic_float() -> Vec<u8> {
        let original = wave();
        let mut bytes = original[..12].to_vec();
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        let mut fields = original[20..36].to_vec();
        fields[..2].copy_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&fields);
        bytes.extend_from_slice(&original[60..]);
        let riff = bytes.len() as u32 - 8;
        bytes[4..8].copy_from_slice(&riff.to_le_bytes());
        bytes
    }
    fn with_odd_chunk() -> Vec<u8> {
        let original = wave();
        let mut bytes = original[..60].to_vec();
        bytes.extend_from_slice(b"JUNK");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[9, 0]);
        bytes.extend_from_slice(&original[60..]);
        let riff = bytes.len() as u32 - 8;
        bytes[4..8].copy_from_slice(&riff.to_le_bytes());
        bytes
    }
    #[test]
    fn valid_and_independent_invalid_metadata_refuse_before_read() {
        let (m, p) = tiny();
        assert_eq!(
            reconstruct_bounded(&m, 2, |_| Ok(p.clone())).unwrap(),
            wave()
        );
        for bytes in [classic_float(), with_odd_chunk()] {
            let (m, p) = fixture(&bytes);
            assert_eq!(
                reconstruct_bounded(&m, 2, |_| Ok(p.clone()))
                    .unwrap_or_else(|e| panic!("valid WAV length {}: {e}", bytes.len())),
                bytes
            );
        }
        let mut cases = Vec::new();
        macro_rules! bad {
            ($field:ident,$value:expr) => {{
                let mut c = m.clone();
                c.$field = $value;
                cases.push(c);
            }};
        }
        bad!(version, 2);
        bad!(frames, 3);
        bad!(rate, 44100);
        bad!(channels, 1);
        bad!(format, "pcm24".into());
        bad!(size, usize::MAX);
        bad!(size, 0);
        bad!(chunks, vec![]);
        for name in ["../000.zlib", "001.zlib", "/000.zlib"] {
            let mut c = m.clone();
            c.chunks[0].name = name.into();
            cases.push(c);
        }
        let mut c = m.clone();
        c.chunks.push(c.chunks[0].clone());
        cases.push(c);
        let mut c = m.clone();
        c.chunks[0].size += 1;
        cases.push(c);
        for c in cases {
            assert!(
                reconstruct_bounded(&c, 2, |_| panic!("metadata must fail before read")).is_err()
            );
        }
    }
    #[test]
    fn missing_truncated_corrupt_expanded_trailing_and_hashes_refused() {
        let (m, p) = tiny();
        assert!(reconstruct_bounded(&m, 2, |_| Err("missing".into())).is_err());
        for b in [
            p[..p.len() - 1].to_vec(),
            vec![0; 10],
            packed(&vec![7; m.size + 1]),
            packed(&vec![7; m.size - 1]),
            {
                let mut b = p.clone();
                b.push(0);
                b
            },
        ] {
            assert!(reconstruct_bounded(&m, 2, |_| Ok(b.clone())).is_err());
        }
        let mut c = m.clone();
        c.sha256 = "0".repeat(64);
        assert!(reconstruct_bounded(&c, 2, |_| Ok(p.clone())).is_err());
        let mut c = m;
        c.chunks[0].sha256 = "0".repeat(64);
        assert!(reconstruct_bounded(&c, 2, |_| Ok(p.clone())).is_err());

        // Hashes are deliberately recalculated for each invalid payload. Only WAV
        // structure/metadata admission can reject these, not the integrity checks.
        let valid = wave();
        let fmt = valid.windows(4).position(|s| s == b"fmt ").unwrap();
        let data = valid.windows(4).position(|s| s == b"data").unwrap();
        let mut pcm = classic_float();
        pcm[20..22].copy_from_slice(&1u16.to_le_bytes());
        let mut short = classic_float();
        short.truncate(short.len() - 8);
        short[40..44].copy_from_slice(&8u32.to_le_bytes());
        let riff = short.len() as u32 - 8;
        short[4..8].copy_from_slice(&riff.to_le_bytes());
        let mut invalid = vec![vec![0; valid.len()], pcm, short];
        for (offset, value) in [(36, 0u8), (38, 0), (40, 1), (44, 1)] {
            let mut bad = valid.clone();
            bad[offset] = value;
            invalid.push(bad);
        }
        let mut no_padding = with_odd_chunk();
        no_padding.remove(69);
        let riff = no_padding.len() as u32 - 8;
        no_padding[4..8].copy_from_slice(&riff.to_le_bytes());
        invalid.push(no_padding);
        let mut bad = valid.clone();
        bad[0..4].copy_from_slice(b"RIFX");
        invalid.push(bad);
        let mut bad = valid.clone();
        bad[8..12].copy_from_slice(b"NOPE");
        invalid.push(bad);
        let mut bad = valid.clone();
        bad[4..8].copy_from_slice(&0u32.to_le_bytes());
        invalid.push(bad);
        let mut bad = valid.clone();
        bad[fmt + 10..fmt + 12].copy_from_slice(&1u16.to_le_bytes());
        invalid.push(bad);
        let mut bad = valid.clone();
        bad[fmt + 12..fmt + 16].copy_from_slice(&44100u32.to_le_bytes());
        invalid.push(bad);
        let mut bad = valid.clone();
        bad[fmt + 16..fmt + 20].copy_from_slice(&1u32.to_le_bytes());
        invalid.push(bad);
        let mut bad = valid.clone();
        bad[fmt + 20..fmt + 22].copy_from_slice(&1u16.to_le_bytes());
        invalid.push(bad);
        let mut bad = valid.clone();
        bad[fmt + 22..fmt + 24].copy_from_slice(&16u16.to_le_bytes());
        invalid.push(bad);
        let mut bad = valid.clone();
        bad[data + 4..data + 8].copy_from_slice(&8u32.to_le_bytes());
        bad[data + 16..].fill(0);
        invalid.push(bad);
        let mut bad = valid.clone();
        bad.extend_from_slice(&[0]);
        let riff = bad.len() as u32 - 8;
        bad[4..8].copy_from_slice(&riff.to_le_bytes());
        invalid.push(bad);
        let mut bad = valid.clone();
        bad.extend_from_slice(&valid[data..]);
        let riff = bad.len() as u32 - 8;
        bad[4..8].copy_from_slice(&riff.to_le_bytes());
        invalid.push(bad);
        for bytes in invalid {
            let (m, p) = fixture(&bytes);
            assert!(
                reconstruct_bounded(&m, 2, |_| Ok(p.clone())).is_err(),
                "matching hashes admitted invalid WAV"
            );
        }
    }
}
