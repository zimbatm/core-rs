//! Integration tests for tarextract: extracts the golden `tar_go.tar`
//! (produced by the Go implementation) into a tempdir and verifies the
//! unprivileged-creatable subset — regular file contents, symlink target,
//! fifo, nanosecond mtimes (including negative), permissions, and xattrs
//! where the platform allows — plus a public-API export→extract round trip.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;

use amber_store_core::fstree::{self, Entry};
use amber_store_core::key::Key;
use amber_store_core::{tarexport, tarextract};

use common::data;

fn mt(s: i64, ns: i64) -> i64 {
    s * 1_000_000_000 + ns
}

fn lstat(p: &Path) -> fs::Metadata {
    fs::symlink_metadata(p).unwrap_or_else(|e| panic!("lstat {}: {e}", p.display()))
}

fn assert_mtime(p: &Path, want_ns: i64) {
    let m = lstat(p);
    let got = m.mtime() * 1_000_000_000 + m.mtime_nsec();
    assert_eq!(got, want_ns, "mtime of {}", p.display());
}

fn is_root() -> bool {
    // SAFETY: geteuid takes no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[test]
fn golden_tar_extracts() {
    let Some(tar) = common::load("tar_go.tar") else {
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("out");
    tarextract::extract(&mut &tar[..], &dest).expect("Extract");

    // Regular files: content and metadata (uid/gid only restorable as root).
    for (name, want) in [
        ("empty", data(0, 0)),
        ("small.txt", data(1, 100)),
        ("medium.bin", data(2, 30000)),
        ("big.bin", data(3, 5242880)),
        ("constant.dat", vec![0xAA; 300000]),
        ("A-upper", data(4, 10)),
        ("é-utf8", data(5, 10)),
    ] {
        let p = dest.join(name);
        assert_eq!(fs::read(&p).expect(name), want, "content of {name}");
        let m = lstat(&p);
        assert_eq!(m.mode() & 0o7777, 0o644, "mode of {name}");
        assert_mtime(&p, 0);
    }

    // Directories, with their (deferred) metadata.
    for d in ["sub", "bigdir"] {
        let m = lstat(&dest.join(d));
        assert!(m.is_dir());
        assert_eq!(m.mode() & 0o7777, 0o755, "mode of {d}");
        assert_mtime(&dest.join(d), 0);
    }

    // bigdir: all 3000 entries restored with exact mtimes; spot-check data.
    assert_eq!(fs::read_dir(dest.join("bigdir")).unwrap().count(), 3000);
    for i in [0u64, 1, 49, 1234, 2999] {
        let p = dest.join("bigdir").join(format!("e{i:06}"));
        assert_eq!(
            fs::read(&p).unwrap(),
            data(1000 + i, (i % 50) as usize),
            "bigdir e{i:06}"
        );
        assert_mtime(&p, mt(1_700_000_000 + i as i64, i as i64));
    }

    // Symlink: target and lstat mtime (with sub-second precision).
    let ln = dest.join("sub").join("ln");
    let m = lstat(&ln);
    assert!(m.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&ln).unwrap().as_os_str().as_bytes(),
        b"../small.txt"
    );
    assert_mtime(&ln, mt(1_600_000_000, 500));

    // Fifo.
    let fifo = dest.join("sub").join("fifo");
    let m = lstat(&fifo);
    assert!(m.file_type().is_fifo(), "sub/fifo is a fifo");
    assert_eq!(m.mode() & 0o7777, 0o644);
    assert_mtime(&fifo, mt(1_600_000_001, 0));

    // Sockets are not archived at all.
    assert!(!dest.join("sub").join("sock").exists(), "socket skipped");

    // Devices need privileges; without them tarextract warns and skips.
    for dev in ["chr", "blk"] {
        let p = dest.join("sub").join(dev);
        if is_root() {
            assert!(p.exists(), "sub/{dev} created when running as root");
        } else {
            assert!(!p.exists(), "sub/{dev} skipped without privileges");
        }
    }

    // Negative nanosecond mtime survives the PAX round trip.
    let old = dest.join("sub").join("old");
    assert_eq!(fs::read(&old).unwrap(), data(11, 10));
    assert_mtime(&old, mt(-1, 999_999_999));

    // Setuid bit restored (ownership only as root).
    let setuid = dest.join("sub").join("setuid");
    assert_eq!(lstat(&setuid).mode() & 0o7777, 0o4755);
    if is_root() {
        let m = lstat(&setuid);
        assert_eq!((m.uid(), m.gid()), (4294967294, 4294967294));
    }

    // Xattrs (best-effort; both these files restore on macOS and on Linux
    // filesystems with user xattrs enabled).
    let xi = dest.join("sub").join("xattr-inline");
    assert_eq!(lstat(&xi).mode() & 0o7777, 0o600);
    assert_mtime(&xi, mt(1_500_000_000, 123_456_789));
    if let Some(v) = xattr::get(&xi, "user.a").expect("xattr get") {
        assert_eq!(v, data(7, 5), "user.a");
        assert_eq!(
            xattr::get(&xi, "user.b").unwrap().expect("user.b"),
            data(8, 100)
        );
        let xs = dest.join("sub").join("xattr-spilled");
        assert_eq!(
            xattr::get(&xs, "user.big").unwrap().expect("user.big"),
            data(10, 400)
        );
    }
    assert_mtime(
        &dest.join("sub").join("xattr-spilled"),
        mt(1_500_000_001, 0),
    );
}

/// Export→extract round trip through the public API only: builds a small
/// tree with the fstree encoders, streams it as tar, extracts it, and
/// verifies the restored filesystem state.
#[test]
fn export_extract_roundtrip() {
    let mut objs: BTreeMap<Key, Vec<u8>> = BTreeMap::new();
    let mut put = |o: fstree::Object| -> Key {
        objs.insert(o.key, o.bytes);
        o.key
    };

    let hello = put(fstree::encode_blob(b"hello roundtrip"));
    let mut xm = BTreeMap::new();
    xm.insert(b"user.roundtrip".to_vec(), b"value\x00binary".to_vec());
    let xattrs_in = amber_store_core::cbor::encode_xattrs(&xm);

    let inner = put(fstree::encode_dir_leaf(&[Entry {
        name: "fïle".as_bytes().to_vec(), // non-ASCII ⇒ PAX path record
        mode: 0o100640,
        uid: 12,
        gid: 34,
        mtime: mt(1_234_567_890, 987_654_321),
        content_key: hello.as_bytes().to_vec(),
        xattrs_in,
        ..Default::default()
    }])
    .expect("inner leaf"));

    let root = put(fstree::encode_dir_leaf(&[
        Entry {
            name: b"dir".to_vec(),
            mode: 0o40711,
            mtime: mt(1_000_000_000, 1),
            content_key: inner.as_bytes().to_vec(),
            ..Default::default()
        },
        Entry {
            name: b"fifo".to_vec(),
            mode: 0o10600,
            mtime: mt(-5, 5),
            ..Default::default()
        },
        Entry {
            name: b"link".to_vec(),
            mode: 0o120777,
            mtime: mt(7, 0),
            link_target: b"dir/does-not-need-to-exist".to_vec(),
            ..Default::default()
        },
    ])
    .expect("root leaf"));

    let mut tar = Vec::new();
    tarexport::write(&mut tar, root, |k| {
        objs.get(&k)
            .cloned()
            .ok_or_else(|| format!("object {k} not in store"))
    })
    .expect("export");

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("restored");
    tarextract::extract(&mut &tar[..], &dest).expect("extract");

    let file = dest.join("dir").join("fïle");
    assert_eq!(fs::read(&file).unwrap(), b"hello roundtrip");
    assert_eq!(lstat(&file).mode() & 0o7777, 0o640);
    assert_mtime(&file, mt(1_234_567_890, 987_654_321));
    if let Some(v) = xattr::get(&file, "user.roundtrip").expect("xattr get") {
        assert_eq!(v, b"value\x00binary");
    }

    let dir = dest.join("dir");
    assert_eq!(lstat(&dir).mode() & 0o7777, 0o711, "deferred dir mode");
    assert_mtime(&dir, mt(1_000_000_000, 1));

    let fifo = dest.join("fifo");
    assert!(lstat(&fifo).file_type().is_fifo());
    assert_eq!(lstat(&fifo).mode() & 0o7777, 0o600);
    assert_mtime(&fifo, mt(-5, 5));

    let link = dest.join("link");
    assert!(lstat(&link).file_type().is_symlink());
    assert_eq!(
        fs::read_link(&link).unwrap().as_os_str().as_bytes(),
        b"dir/does-not-need-to-exist"
    );
    assert_mtime(&link, mt(7, 0));

    // Restore search permission on "dir" (0o711 already allows traversal,
    // but be explicit for cleanup robustness).
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
}
