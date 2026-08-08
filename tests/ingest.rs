//! Public-API integration coverage for `ingest`: the object stream, the
//! packstore-backed build, `.amberignore` handling and the sizing scan, all
//! through the crate's external surface (the in-module tests port the full Go
//! suite against the internals).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use amber_store_core::ingest::{self, Opts, Progress};
use amber_store_core::packstore;
use tempfile::TempDir;

fn write_file(path: &Path, content: &[u8], mode: u32) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A small fixture: files, a subdirectory, a symlink, and an `.amberignore`
/// that excludes one file.
fn write_fixture(dir: &Path) {
    write_file(&dir.join(".amberignore"), b"*.log\n", 0o644);
    write_file(&dir.join("a.txt"), b"alpha", 0o644);
    write_file(&dir.join("drop.log"), b"dropped", 0o644);
    let sub = dir.join("sub");
    fs::create_dir(&sub).unwrap();
    write_file(&sub.join("b.txt"), b"beta", 0o600);
    std::os::unix::fs::symlink("a.txt", dir.join("link")).unwrap();
}

#[derive(Default)]
struct Counting {
    files: AtomicU64,
    bytes: AtomicU64,
}

impl Progress for Counting {
    fn file_done(&self) {
        self.files.fetch_add(1, Ordering::SeqCst);
    }
    fn add_bytes(&self, n: usize) {
        self.bytes.fetch_add(n as u64, Ordering::SeqCst);
    }
}

#[test]
fn public_api_end_to_end() {
    let src = TempDir::new().unwrap();
    write_fixture(src.path());

    // objects(): stream drains, root resolves, root object is in the stream.
    let p = Arc::new(Counting::default());
    let (stream, root) = ingest::objects(
        src.path(),
        Opts {
            jobs: 4,
            progress: Some(p.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        root.get().is_none(),
        "root must not resolve before draining"
    );
    let mut objs = std::collections::HashMap::new();
    for r in stream {
        let o = r.unwrap();
        objs.insert(o.key, o.bytes);
    }
    let root = root.get().expect("root set after drain");
    assert!(objs.contains_key(&root), "stream carries the root object");

    // The scan sizes exactly what the build read (the .log file is ignored).
    let (files, bytes) = ingest::scan(src.path(), false, 4).unwrap();
    assert_eq!(files, p.files.load(Ordering::SeqCst), "scan files");
    assert_eq!(bytes, p.bytes.load(Ordering::SeqCst), "scan bytes");

    // dir(): same root, objects land in the packstore.
    let store_dir = TempDir::new().unwrap();
    let st = packstore::Store::open(store_dir.path().join("packstore")).unwrap();
    let (stats, res) = ingest::dir(&st, src.path(), Opts::default());
    let stored_root = res.unwrap();
    assert_eq!(stored_root, root, "dir root != objects root");
    assert!(stats.stored > 0, "no objects stored");
    st.get(stored_root).expect("root retrievable");
    st.close().unwrap();

    // no_ignore folds the excluded file back in and changes the root.
    let (stream, no_ignore_root) = ingest::objects(
        src.path(),
        Opts {
            no_ignore: true,
            ..Default::default()
        },
    )
    .unwrap();
    for r in stream {
        r.unwrap();
    }
    assert_ne!(
        no_ignore_root.get().expect("root set"),
        root,
        "no_ignore must change the root"
    );
}

#[test]
fn single_file_root_is_content_key() {
    let tmp = TempDir::new().unwrap();
    let f = tmp.path().join("f.bin");
    write_file(&f, &vec![7u8; 100_000], 0o644);
    let (stream, root) = ingest::objects(&f, Opts::default()).unwrap();
    for r in stream {
        r.unwrap();
    }
    let root = root.get().expect("root set");
    assert_eq!(root.length(), 100_000, "file root key carries the length");
}

#[test]
fn early_drop_aborts_build() {
    let tmp = TempDir::new().unwrap();
    for i in 0..64 {
        write_file(
            &tmp.path().join(format!("f{i:02}")),
            format!("content {i}").as_bytes(),
            0o644,
        );
    }
    let (mut stream, root) = ingest::objects(
        tmp.path(),
        Opts {
            jobs: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let first = stream.next().expect("at least one object");
    first.unwrap();
    drop(stream); // consumer stops early: the build must unwind, not error
    assert!(
        root.get().is_none(),
        "aborted build must not resolve a root"
    );
}
