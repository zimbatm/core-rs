//! Shared support for golden-vector integration tests. See VECTORS.md.
#![allow(dead_code)]

use std::path::PathBuf;

/// splitmix64 as defined in VECTORS.md.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// `data(seed, n)` from VECTORS.md: little-endian u64 stream, truncated.
pub fn data(seed: u64, n: usize) -> Vec<u8> {
    let mut rng = SplitMix64(seed);
    let mut out = Vec::with_capacity(n + 8);
    while out.len() < n {
        out.extend_from_slice(&rng.next().to_le_bytes());
    }
    out.truncate(n);
    out
}

/// `u64s(seed, n)` from VECTORS.md.
pub fn u64s(seed: u64, n: usize) -> Vec<u64> {
    let mut rng = SplitMix64(seed);
    (0..n).map(|_| rng.next()).collect()
}

/// A payload description as it appears in the vector JSON files.
#[derive(serde::Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Payload {
    Splitmix { seed: u64, len: usize },
    Const { byte: u8, len: usize },
    Concat { parts: Vec<Payload> },
}

impl Payload {
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Payload::Splitmix { seed, len } => data(*seed, *len),
            Payload::Const { byte, len } => vec![*byte; *len],
            Payload::Concat { parts } => parts.iter().flat_map(|p| p.bytes()).collect(),
        }
    }
}

/// Root of the committed golden vectors.
pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Load a golden file. Missing files panic — the vectors are committed — unless
/// AMBER_GOLDEN_OPTIONAL=1 is set (pre-generation development), in which case
/// the test should return early on `None`.
pub fn load(rel: &str) -> Option<Vec<u8>> {
    let p = golden_dir().join(rel);
    match std::fs::read(&p) {
        Ok(b) => Some(b),
        Err(e) if std::env::var_os("AMBER_GOLDEN_OPTIONAL").is_some() => {
            eprintln!("skipping: golden vector {} unavailable: {e}", p.display());
            None
        }
        Err(e) => panic!(
            "golden vector {} missing: {e} (run tools/vectorgen)",
            p.display()
        ),
    }
}

/// Parse a decimal string field that may exceed 2^53 (JSON-safe integer range).
pub fn parse_u64(s: &str) -> u64 {
    s.parse().expect("decimal u64 string")
}

pub fn parse_i64(s: &str) -> i64 {
    s.parse().expect("decimal i64 string")
}
