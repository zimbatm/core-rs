//! Golden-vector test for the fstree builders + read paths: builds the ENTIRE
//! golden tree of `tests/golden/fstree/` (VECTORS.md) through the public
//! builder APIs and asserts the root key and the deduplicated emitted object
//! set (keys AND bytes) match the Go-generated manifest exactly, then
//! exercises every read path against the built tree.

mod common;

use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
use std::sync::LazyLock;

use amber_store_core::cbor;
use amber_store_core::chunkers::{ItemChunker, split_bytes};
use amber_store_core::fstree::{
    self, DirBuilder, Entry, IndexBuilder, Object, check_complete, collect_entries, list_entries,
    lookup_entry, reachable_keys, resolve_entry, resolve_path, write_content,
};
use amber_store_core::key::{Key, SIZE, Type};

use common::data;

/// The golden tree's chunking parameters (the ingest defaults).
const ITEM_BITS: u32 = 7;
const XATTR_INLINE_MAX: usize = 256;

#[derive(serde::Deserialize)]
struct Manifest {
    root: String,
    objects: Vec<ManifestObject>,
}

#[derive(serde::Deserialize)]
struct ManifestObject {
    key: String,
    size: u64,
}

/// The golden vectors plus the tree rebuilt through the Rust builders.
struct Golden {
    manifest_root: String,
    /// Objects from `objects.bin`, keyed for comparison and reads.
    golden: BTreeMap<Key, Vec<u8>>,
    /// The deduplicated set emitted by the Rust builders.
    built: BTreeMap<Key, Vec<u8>>,
    root: Key,
    sub_root: Key,
    bigdir_root: Key,
}

impl Golden {
    fn get(&self) -> impl Fn(Key) -> Result<Vec<u8>, String> + Sync + '_ {
        |k| {
            self.built
                .get(&k)
                .cloned()
                .ok_or_else(|| format!("object {k} not in store"))
        }
    }
}

static GOLDEN: LazyLock<Option<Golden>> = LazyLock::new(load_and_build);

fn load_and_build() -> Option<Golden> {
    let manifest: Manifest =
        serde_json::from_slice(&common::load("fstree/manifest.json")?).expect("manifest parses");
    let bin = common::load("fstree/objects.bin")?;

    // objects.bin: key (32 bytes) ‖ big-endian u64 length ‖ object bytes.
    let mut golden = BTreeMap::new();
    let mut off = 0usize;
    while off < bin.len() {
        assert!(bin.len() - off >= 40, "truncated record header at {off}");
        let key = Key::parse(&bin[off..off + 32]).expect("golden key parses");
        let mut lenbuf = [0u8; 8];
        lenbuf.copy_from_slice(&bin[off + 32..off + 40]);
        let len = u64::from_be_bytes(lenbuf) as usize;
        off += 40;
        assert!(bin.len() - off >= len, "truncated object body at {off}");
        golden.insert(key, bin[off..off + len].to_vec());
        off += len;
    }
    assert_eq!(
        golden.len(),
        manifest.objects.len(),
        "duplicate golden keys"
    );
    for m in &manifest.objects {
        let k = Key::parse(&hex::decode(&m.key).expect("manifest key hex")).expect("manifest key");
        let bytes = golden.get(&k).expect("manifest key present in objects.bin");
        assert_eq!(bytes.len() as u64, m.size, "manifest size for {k}");
    }

    let (built, root, sub_root, bigdir_root) = build_golden_tree();
    Some(Golden {
        manifest_root: manifest.root,
        golden,
        built,
        root,
        sub_root,
        bigdir_root,
    })
}

// --- tree construction (mirrors tools/vectorgen/fstree.go, which mirrors
// --- the Go ingest driver) ---

/// Deduplicating emit sink: consistent re-emissions are dropped, conflicting
/// ones are a bug.
fn emit_into(
    objs: &mut BTreeMap<Key, Vec<u8>>,
) -> impl FnMut(Object) -> Result<(), Infallible> + '_ {
    |o| {
        if let Some(prev) = objs.get(&o.key) {
            assert_eq!(*prev, o.bytes, "key {} emitted with differing bytes", o.key);
        } else {
            objs.insert(o.key, o.bytes);
        }
        Ok(())
    }
}

/// Mirrors ingest's `driver.buildFile`: split content with the default
/// ultracdc parameters, encode each chunk as a Blob, feed the blob keys
/// through a file IndexBuilder (which returns a single blob's key unwrapped,
/// and builds FileNode levels above multiple blobs). An empty file is a
/// single empty Blob.
fn build_file(objs: &mut BTreeMap<Key, Vec<u8>>, ic: ItemChunker, content: &[u8]) -> Key {
    let mut ib = IndexBuilder::new_file(ic);
    let mut saw = false;
    split_bytes(content, None, |chunk: Vec<u8>| -> Result<(), Infallible> {
        saw = true;
        let obj = fstree::encode_blob(&chunk);
        let k = obj.key;
        let mut em = emit_into(objs);
        em(obj).unwrap();
        ib.add_child(&mut em, k, &[]).unwrap();
        Ok(())
    })
    .expect("split_bytes");
    if !saw {
        let obj = fstree::encode_blob(&[]);
        let k = obj.key;
        let mut em = emit_into(objs);
        em(obj).unwrap();
        ib.add_child(&mut em, k, &[]).unwrap();
    }
    let mut em = emit_into(objs);
    ib.finish(&mut em).expect("file index finish")
}

/// Feeds entries (sorted bytewise by name here) through a DirBuilder and
/// returns the directory's root key.
fn build_dir(objs: &mut BTreeMap<Key, Vec<u8>>, ic: ItemChunker, mut entries: Vec<Entry>) -> Key {
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut db = DirBuilder::new(ic);
    for e in entries {
        let mut em = emit_into(objs);
        db.add_entry(&mut em, e).unwrap();
    }
    let mut em = emit_into(objs);
    db.finish(&mut em).expect("dir finish")
}

/// Applies ingest's inline-vs-spill rule (`driver.buildEntry`): the canonical
/// CBOR xattr map stays inline iff its encoding is ≤ 256 bytes, otherwise it
/// is emitted as an XattrSet object referenced by key 9.
fn set_xattrs(
    objs: &mut BTreeMap<Key, Vec<u8>>,
    e: &mut Entry,
    xattrs: BTreeMap<Vec<u8>, Vec<u8>>,
) {
    if xattrs.is_empty() {
        return;
    }
    let enc = cbor::encode_xattrs(&xattrs);
    if enc.len() <= XATTR_INLINE_MAX {
        e.xattrs_in = enc;
        return;
    }
    let obj = fstree::encode_xattr_set(&xattrs);
    let k = obj.key;
    emit_into(objs)(obj).unwrap();
    e.xattrs_key = k.as_bytes().to_vec();
}

/// `mt(s, ns)` from VECTORS.md: `s*1e9 + ns` nanoseconds.
fn mt(s: i64, ns: i64) -> i64 {
    s * 1_000_000_000 + ns
}

#[allow(clippy::too_many_arguments)]
fn file_entry(
    objs: &mut BTreeMap<Key, Vec<u8>>,
    ic: ItemChunker,
    name: &[u8],
    content: &[u8],
    mode: u64,
    uid: u64,
    gid: u64,
    mtime: i64,
    xattrs: BTreeMap<Vec<u8>, Vec<u8>>,
) -> Entry {
    let ck = build_file(objs, ic, content);
    let mut e = Entry {
        name: name.to_vec(),
        mode,
        uid,
        gid,
        mtime,
        content_key: ck.as_bytes().to_vec(),
        ..Default::default()
    };
    set_xattrs(objs, &mut e, xattrs);
    e
}

/// Constructs the VECTORS.md golden tree in memory through the public fstree
/// builder APIs; returns (objects, root, sub root, bigdir root).
fn build_golden_tree() -> (BTreeMap<Key, Vec<u8>>, Key, Key, Key) {
    let ic = ItemChunker::new(ITEM_BITS);
    let mut objs: BTreeMap<Key, Vec<u8>> = BTreeMap::new();
    let o = &mut objs;

    // sub: special files and metadata edge cases (uid 0, gid 0 unless
    // stated; any metadata not stated is zero).
    let mut sub_entries = vec![
        Entry {
            name: b"ln".to_vec(),
            mode: 0o120777,
            mtime: mt(1600000000, 500),
            link_target: b"../small.txt".to_vec(),
            ..Default::default()
        },
        Entry {
            name: b"fifo".to_vec(),
            mode: 0o10644,
            mtime: mt(1600000001, 0),
            ..Default::default()
        },
        Entry {
            name: b"sock".to_vec(),
            mode: 0o140644,
            mtime: mt(1600000002, 0),
            ..Default::default()
        },
        Entry {
            name: b"chr".to_vec(),
            mode: 0o20644,
            mtime: mt(1600000003, 0),
            rdev: vec![1, 3],
            ..Default::default()
        },
        Entry {
            name: b"blk".to_vec(),
            mode: 0o60644,
            mtime: mt(1600000004, 0),
            rdev: vec![259, 0],
            ..Default::default()
        },
    ];
    let e = file_entry(
        o,
        ic,
        b"xattr-inline",
        &data(6, 50),
        0o100600,
        501,
        20,
        mt(1500000000, 123456789),
        BTreeMap::from([
            (b"user.a".to_vec(), data(7, 5)),
            (b"user.b".to_vec(), data(8, 100)),
        ]),
    );
    assert!(
        !e.xattrs_in.is_empty() && e.xattrs_key.is_empty(),
        "xattr-inline: expected inline xattrs"
    );
    sub_entries.push(e);
    let e = file_entry(
        o,
        ic,
        b"xattr-spilled",
        &data(9, 50),
        0o100644,
        0,
        0,
        mt(1500000001, 0),
        BTreeMap::from([(b"user.big".to_vec(), data(10, 400))]),
    );
    assert!(
        e.xattrs_key.len() == SIZE && e.xattrs_in.is_empty(),
        "xattr-spilled: expected spilled xattrs"
    );
    sub_entries.push(e);
    sub_entries.push(file_entry(
        o,
        ic,
        b"old",
        &data(11, 10),
        0o100644,
        0,
        0,
        mt(-1, 999999999),
        BTreeMap::new(),
    ));
    sub_entries.push(file_entry(
        o,
        ic,
        b"setuid",
        &data(12, 10),
        0o104755,
        4294967294,
        4294967294,
        0,
        BTreeMap::new(),
    ));
    let sub_root = build_dir(o, ic, sub_entries);

    // bigdir: 3000 regular files e%06d, content data(1000+i, i mod 50)
    // (60 of them empty ⇒ dedup), mt(1700000000+i, i).
    let mut bigdir_entries = Vec::with_capacity(3000);
    for i in 0..3000u64 {
        bigdir_entries.push(file_entry(
            o,
            ic,
            format!("e{i:06}").as_bytes(),
            &data(1000 + i, (i % 50) as usize),
            0o100644,
            1000,
            1000,
            mt(1700000000 + i as i64, i as i64),
            BTreeMap::new(),
        ));
    }
    let bigdir_root = build_dir(o, ic, bigdir_entries);

    // Root entries (files mode 0o100644, dirs 0o40755, uid/gid 1000/1000,
    // unstated mtimes 0 — including the sub/bigdir directory entries).
    let root_files: &[(&[u8], Vec<u8>)] = &[
        (b"empty", data(0, 0)),
        (b"small.txt", data(1, 100)),
        (b"medium.bin", data(2, 30000)),
        (b"big.bin", data(3, 5242880)),
        (b"constant.dat", vec![0xAA; 300000]),
        (b"A-upper", data(4, 10)),
        ("\u{e9}-utf8".as_bytes(), data(5, 10)),
    ];
    let mut root_entries = Vec::new();
    for (name, content) in root_files {
        root_entries.push(file_entry(
            o,
            ic,
            name,
            content,
            0o100644,
            1000,
            1000,
            0,
            BTreeMap::new(),
        ));
    }
    root_entries.push(Entry {
        name: b"sub".to_vec(),
        mode: 0o40755,
        uid: 1000,
        gid: 1000,
        content_key: sub_root.as_bytes().to_vec(),
        ..Default::default()
    });
    root_entries.push(Entry {
        name: b"bigdir".to_vec(),
        mode: 0o40755,
        uid: 1000,
        gid: 1000,
        content_key: bigdir_root.as_bytes().to_vec(),
        ..Default::default()
    });
    let root = build_dir(o, ic, root_entries);

    (objs, root, sub_root, bigdir_root)
}

// --- the golden assertions ---

#[test]
fn golden_tree_root_and_object_set() {
    let Some(g) = GOLDEN.as_ref() else { return };

    assert_eq!(
        g.root.to_string(),
        g.manifest_root,
        "root key differs from the Go-built manifest root"
    );

    // The emitted deduplicated object set must equal the manifest set
    // EXACTLY: every key and every object's bytes.
    for (k, bytes) in &g.golden {
        match g.built.get(k) {
            None => panic!("golden object {k} was not emitted by the Rust builders"),
            Some(b) => assert_eq!(b, bytes, "object {k}: bytes differ"),
        }
    }
    for k in g.built.keys() {
        assert!(
            g.golden.contains_key(k),
            "Rust builders emitted {k}, absent from the manifest"
        );
    }
    assert_eq!(g.built.len(), g.golden.len());

    // The subtree roots are directory objects inside the set.
    for k in [g.sub_root, g.bigdir_root] {
        assert!(matches!(k.type_(), Type::DirLeaf | Type::DirNode));
        assert!(g.golden.contains_key(&k));
    }
}

#[test]
fn golden_tree_lookup_samples() {
    let Some(g) = GOLDEN.as_ref() else { return };
    let get = g.get();

    // Root-table entries with their VECTORS.md metadata.
    let empty = lookup_entry(g.root, b"empty", &get).unwrap();
    assert_eq!(
        (empty.mode, empty.uid, empty.gid, empty.mtime),
        (0o100644, 1000, 1000, 0)
    );
    let ck = Key::parse(&empty.content_key).unwrap();
    assert_eq!((ck.type_(), ck.length()), (Type::Blob, 0), "empty blob");

    let big = lookup_entry(g.root, b"big.bin", &get).unwrap();
    let big_ck = Key::parse(&big.content_key).unwrap();
    assert_eq!(big_ck.type_(), Type::FileNode, "5 MiB file promotes");
    assert_eq!(big_ck.length(), 5242880, "FileNode length = content bytes");

    let upper = lookup_entry(g.root, b"A-upper", &get).unwrap();
    assert_eq!(upper.mode, 0o100644);

    let utf8 = lookup_entry(g.root, "\u{e9}-utf8".as_bytes(), &get).unwrap();
    assert_eq!(Key::parse(&utf8.content_key).unwrap().length(), 10);

    let sub = lookup_entry(g.root, b"sub", &get).unwrap();
    assert_eq!(sub.content_key, g.sub_root.as_bytes().to_vec());
    assert_eq!((sub.mode, sub.mtime), (0o40755, 0));

    let bigdir = lookup_entry(g.root, b"bigdir", &get).unwrap();
    assert_eq!(bigdir.content_key, g.bigdir_root.as_bytes().to_vec());

    // sub entries.
    let ln = lookup_entry(g.sub_root, b"ln", &get).unwrap();
    assert_eq!(ln.link_target, b"../small.txt");
    assert_eq!((ln.mode, ln.mtime), (0o120777, mt(1600000000, 500)));
    assert!(ln.content_key.is_empty());

    let chr = lookup_entry(g.sub_root, b"chr", &get).unwrap();
    assert_eq!(chr.rdev, vec![1, 3]);

    let old = lookup_entry(g.sub_root, b"old", &get).unwrap();
    assert_eq!(old.mtime, mt(-1, 999999999), "negative mtime survives");

    let setuid = lookup_entry(g.sub_root, b"setuid", &get).unwrap();
    assert_eq!((setuid.uid, setuid.gid), (4294967294, 4294967294));

    let spilled = lookup_entry(g.sub_root, b"xattr-spilled", &get).unwrap();
    let xk = Key::parse(&spilled.xattrs_key).unwrap();
    assert_eq!(xk.type_(), Type::XattrSet);
    let xset = cbor::decode_xattrs(&g.built[&xk]).unwrap();
    assert_eq!(
        xset,
        BTreeMap::from([(b"user.big".to_vec(), data(10, 400))])
    );

    let inline = lookup_entry(g.sub_root, b"xattr-inline", &get).unwrap();
    assert_eq!(
        cbor::decode_xattrs(&inline.xattrs_in).unwrap(),
        BTreeMap::from([
            (b"user.a".to_vec(), data(7, 5)),
            (b"user.b".to_vec(), data(8, 100)),
        ])
    );

    // bigdir entries, resolved through the multi-level DirNode index.
    for i in [0u64, 1, 37, 500, 1500, 2049, 2998, 2999] {
        let name = format!("e{i:06}");
        let ent = lookup_entry(g.bigdir_root, name.as_bytes(), &get)
            .unwrap_or_else(|e| panic!("lookup {name}: {e}"));
        assert_eq!(ent.mtime, mt(1700000000 + i as i64, i as i64), "{name}");
        let ck = Key::parse(&ent.content_key).unwrap();
        assert_eq!(ck.length(), i % 50, "{name} content length");
    }
    assert!(
        lookup_entry(g.bigdir_root, b"e003000", &get)
            .unwrap_err()
            .is_not_found()
    );
    assert!(
        lookup_entry(g.root, b"nope", &get)
            .unwrap_err()
            .is_not_found()
    );
}

#[test]
fn golden_tree_list_pagination_sweep() {
    let Some(g) = GOLDEN.as_ref() else { return };
    let get = g.get();

    // The full listing in name order, via the recursive collector.
    let all = collect_entries(g.bigdir_root, &get).unwrap();
    assert_eq!(all.len(), 3000);
    for (i, e) in all.iter().enumerate() {
        assert_eq!(e.name, format!("e{i:06}").as_bytes(), "sorted order");
    }

    // Pagination sweeps must reproduce it exactly for every page size.
    for limit in [1usize, 7, 128, 999, 3000, 5000] {
        let mut got: Vec<Entry> = Vec::new();
        let mut after: Vec<u8> = Vec::new();
        loop {
            let (page, more) = list_entries(g.bigdir_root, &after, limit, &get).unwrap();
            assert!(page.len() <= limit);
            if !more {
                got.extend(page);
                break;
            }
            assert_eq!(page.len(), limit, "more=true page must be full");
            after = page.last().unwrap().name.clone();
            got.extend(page);
        }
        assert_eq!(got, all, "limit {limit}");
    }

    // The truncation flag flips exactly at the boundary.
    let (page, more) = list_entries(g.bigdir_root, &[], 3000, &get).unwrap();
    assert_eq!((page.len(), more), (3000, false));
    let (page, more) = list_entries(g.bigdir_root, &[], 2999, &get).unwrap();
    assert_eq!((page.len(), more), (2999, true));
    let (page, more) = list_entries(g.bigdir_root, b"e002999", 10, &get).unwrap();
    assert_eq!((page.len(), more), (0, false));

    // Root directory listing is bytewise-sorted.
    let root_names: Vec<Vec<u8>> = collect_entries(g.root, &get)
        .unwrap()
        .iter()
        .map(|e| e.name.clone())
        .collect();
    let expect: Vec<Vec<u8>> = [
        &b"A-upper"[..],
        b"big.bin",
        b"bigdir",
        b"constant.dat",
        b"empty",
        b"medium.bin",
        b"small.txt",
        b"sub",
        "\u{e9}-utf8".as_bytes(),
    ]
    .iter()
    .map(|n| n.to_vec())
    .collect();
    assert_eq!(root_names, expect);
}

#[test]
fn golden_tree_write_content() {
    let Some(g) = GOLDEN.as_ref() else { return };
    let get = g.get();

    for (name, want) in [
        (&b"big.bin"[..], data(3, 5242880)),
        (b"medium.bin", data(2, 30000)),
        (b"constant.dat", vec![0xAA; 300000]),
        (b"small.txt", data(1, 100)),
        (b"empty", Vec::new()),
    ] {
        let ent = lookup_entry(g.root, name, &get).unwrap();
        let ck = Key::parse(&ent.content_key).unwrap();
        let mut out = Vec::new();
        write_content(&mut out, ck, &get).unwrap();
        assert_eq!(
            out,
            want,
            "{} content mismatch",
            String::from_utf8_lossy(name)
        );
    }
}

#[test]
fn golden_tree_resolve() {
    let Some(g) = GOLDEN.as_ref() else { return };
    let get = g.get();

    assert_eq!(resolve_path(g.root, "", &get).unwrap(), g.root);
    assert_eq!(resolve_path(g.root, "sub", &get).unwrap(), g.sub_root);
    assert_eq!(
        resolve_path(g.root, "/bigdir/", &get).unwrap(),
        g.bigdir_root
    );

    // resolve_entry("sub/ln") returns the symlink entry itself.
    let ln = resolve_entry(g.root, "sub/ln", &get).unwrap().unwrap();
    assert_eq!(ln.link_target, b"../small.txt");
    assert_eq!(ln.mode, 0o120777);

    // resolve_path("sub/ln") names a non-directory: NotDir.
    let err = resolve_path(g.root, "sub/ln", &get).unwrap_err();
    assert!(err.is_not_dir());
    assert_eq!(err.to_string(), "fstree: \"ln\": not a directory");

    // A bigdir file through the DirNode levels.
    let e = resolve_entry(g.root, "bigdir/e002500", &get)
        .unwrap()
        .unwrap();
    assert_eq!(e.mtime, mt(1700000000 + 2500, 2500));

    assert!(
        resolve_entry(g.root, "sub/absent", &get)
            .unwrap_err()
            .is_not_found()
    );
}

#[test]
fn golden_tree_reachable_keys() {
    let Some(g) = GOLDEN.as_ref() else { return };

    let keys = reachable_keys(g.root, g.get()).unwrap();
    assert_eq!(keys[0], g.root, "root first");
    let set: HashSet<Key> = keys.iter().copied().collect();
    assert_eq!(set.len(), keys.len(), "no duplicates");
    let want: HashSet<Key> = g.golden.keys().copied().collect();
    assert_eq!(
        set, want,
        "reachable keys must cover exactly the manifest set"
    );
}

#[test]
fn golden_tree_check_complete() {
    let Some(g) = GOLDEN.as_ref() else { return };

    let has = |k: Key| Ok::<bool, String>(g.built.contains_key(&k));
    check_complete(g.root, g.get(), has, 0).unwrap();
    check_complete(g.root, g.get(), has, 4).unwrap();

    // Delete one leaf object (the empty Blob, shared by "empty" and the 60
    // empty bigdir files): check_complete must name it missing.
    let empty_blob = fstree::encode_blob(&[]).key;
    assert!(g.built.contains_key(&empty_blob), "fixture sanity");
    let mut pruned = g.built.clone();
    pruned.remove(&empty_blob);
    let get = |k: Key| {
        pruned
            .get(&k)
            .cloned()
            .ok_or_else(|| format!("object {k} not in store"))
    };
    let has = |k: Key| Ok::<bool, String>(pruned.contains_key(&k));
    let err = check_complete(g.root, get, has, 4).unwrap_err();
    let miss = err.missing_object().expect("want MissingObjectError");
    assert_eq!(miss.key, empty_blob);
    assert_eq!(
        err.to_string(),
        format!("fstree: object {empty_blob} is missing")
    );

    // Delete an interior node (big.bin's FileNode root): the wrapped get
    // error surfaces instead.
    let big = lookup_entry(g.root, b"big.bin", g.get()).unwrap();
    let big_ck = Key::parse(&big.content_key).unwrap();
    let mut pruned = g.built.clone();
    pruned.remove(&big_ck);
    let get = |k: Key| {
        pruned
            .get(&k)
            .cloned()
            .ok_or_else(|| format!("object {k} not in store"))
    };
    let has = |k: Key| Ok::<bool, String>(pruned.contains_key(&k));
    let err = check_complete(g.root, get, has, 4).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("fstree: reading {big_ck}: object {big_ck} not in store")
    );
}
