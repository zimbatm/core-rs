//! Integration tests for the public `cbor` API surface.
//!
//! This module has no golden vectors of its own — the fstree and reference
//! vectors exercise the encoding byte-for-byte (see VECTORS.md). These tests
//! confirm the primitives are publicly usable and self-consistent.

use std::collections::BTreeMap;

use amber_store_core::cbor;

fn bstr(s: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    cbor::append_bstr(&mut b, s);
    b
}

#[test]
fn public_primitives_roundtrip() {
    let mut b = Vec::new();
    cbor::append_head(&mut b, cbor::MAJOR_ARRAY, 3);
    cbor::append_bstr(&mut b, b"hello");
    let (major, n, rest) = cbor::read_head(&b).unwrap();
    assert_eq!((major, n), (cbor::MAJOR_ARRAY, 3));
    let (val, rest) = cbor::read_bstr(rest).unwrap();
    assert_eq!(val, b"hello");
    assert!(rest.is_empty());
}

#[test]
fn xattr_codec_roundtrip_and_canonical_order() {
    let mut m: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    m.insert(b"user.selinux".to_vec(), b"x".to_vec());
    m.insert(b"a".to_vec(), b"1".to_vec());
    m.insert(b"bb".to_vec(), b"2".to_vec());
    let enc = cbor::encode_xattrs(&m);

    // Canonical bytes: map(3), then pairs ordered by encoded-key bytes.
    let mut want = vec![0xa3];
    want.extend_from_slice(&bstr(b"a"));
    want.extend_from_slice(&bstr(b"1"));
    want.extend_from_slice(&bstr(b"bb"));
    want.extend_from_slice(&bstr(b"2"));
    want.extend_from_slice(&bstr(b"user.selinux"));
    want.extend_from_slice(&bstr(b"x"));
    assert_eq!(enc, want);

    assert_eq!(cbor::decode_xattrs(&enc).unwrap(), m);
}

#[test]
fn decode_rejects_trailing_bytes_and_wrong_major() {
    let mut enc = cbor::encode_xattrs(&BTreeMap::new());
    assert_eq!(enc, vec![0xa0]);
    enc.push(0x00);
    assert_eq!(
        cbor::decode_xattrs(&enc),
        Err(cbor::Error::TrailingBytes(1))
    );
    assert_eq!(
        cbor::decode_xattrs(&[0x40]),
        Err(cbor::Error::ExpectedMap { got: 2 })
    );
}
