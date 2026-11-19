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
    Ok(out)
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
    fn tiny() -> (Manifest, Vec<u8>) {
        let b = vec![7; 16];
        (
            Manifest {
                version: 1,
                frames: 2,
                rate: 48000,
                channels: 2,
                format: "f32le-wav".into(),
                size: 16,
                sha256: hash(&b),
                chunks: vec![Chunk {
                    name: "000.zlib".into(),
                    size: 16,
                    sha256: hash(&b),
                }],
            },
            packed(&b),
        )
    }
    #[test]
    fn valid_and_independent_invalid_metadata_refuse_before_read() {
        let (m, p) = tiny();
        assert_eq!(
            reconstruct_bounded(&m, 2, |_| Ok(p.clone())).unwrap(),
            vec![7; 16]
        );
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
        c.chunks[0].size = 17;
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
            packed(&[7; 17]),
            packed(&[7; 15]),
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
    }
}
