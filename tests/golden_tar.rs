//! Golden-vector test for tarexport: exports the golden fstree of
//! `tests/golden/fstree/` (VECTORS.md) and asserts the stream is
//! BYTE-IDENTICAL to `tests/golden/tar_go.tar`, which was produced by the Go
//! implementation (`tarexport.Write` on top of Go's `archive/tar`).

mod common;

use std::collections::BTreeMap;

use amber_store_core::key::Key;
use amber_store_core::tarexport;

#[derive(serde::Deserialize)]
struct Manifest {
    root: String,
}

/// Parses `fstree/objects.bin`: `key (32) ‖ big-endian u64 length ‖ bytes`.
fn parse_objects(bin: &[u8]) -> BTreeMap<Key, Vec<u8>> {
    let mut objs = BTreeMap::new();
    let mut off = 0usize;
    while off < bin.len() {
        assert!(bin.len() - off >= 40, "truncated record header at {off}");
        let key = Key::parse(&bin[off..off + 32]).expect("golden key parses");
        let mut lenbuf = [0u8; 8];
        lenbuf.copy_from_slice(&bin[off + 32..off + 40]);
        let len = u64::from_be_bytes(lenbuf) as usize;
        off += 40;
        assert!(bin.len() - off >= len, "truncated object body at {off}");
        objs.insert(key, bin[off..off + len].to_vec());
        off += len;
    }
    objs
}

#[test]
fn golden_tar_byte_identity() {
    let Some(manifest_bytes) = common::load("fstree/manifest.json") else {
        return;
    };
    let Some(bin) = common::load("fstree/objects.bin") else {
        return;
    };
    let Some(want) = common::load("tar_go.tar") else {
        return;
    };

    let manifest: Manifest = serde_json::from_slice(&manifest_bytes).expect("manifest parses");
    let objs = parse_objects(&bin);
    let root = Key::parse(&hex::decode(&manifest.root).expect("root hex")).expect("root key");

    let mut got = Vec::with_capacity(want.len());
    tarexport::write(&mut got, root, |k| {
        objs.get(&k)
            .cloned()
            .ok_or_else(|| format!("object {k} not in store"))
    })
    .expect("tarexport::write");

    if got != want {
        // Locate the first differing 512-byte block for a readable failure.
        let blocks = got.len().max(want.len()).div_ceil(512);
        for i in 0..blocks {
            let (a, b) = (slice_block(&got, i), slice_block(&want, i));
            if a != b {
                panic!(
                    "tar output differs from tar_go.tar at block {i} (offset {}):\n\
                     got  ({} bytes total): {}\n\
                     want ({} bytes total): {}",
                    i * 512,
                    got.len(),
                    hex_block(a),
                    want.len(),
                    hex_block(b),
                );
            }
        }
        panic!(
            "tar output differs in length only: got {} bytes, want {}",
            got.len(),
            want.len()
        );
    }
}

fn slice_block(buf: &[u8], i: usize) -> &[u8] {
    let start = (i * 512).min(buf.len());
    let end = ((i + 1) * 512).min(buf.len());
    &buf[start..end]
}

fn hex_block(b: &[u8]) -> String {
    if b.is_empty() {
        return "<missing>".into();
    }
    hex::encode(b)
}
