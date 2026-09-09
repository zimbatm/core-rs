use amber_store_core::{
    key::{Key, Type},
    packstore::{Options, Store, ValidatedIndexDigest},
};
use std::{
    fs::{self, File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::FileExt},
    path::{Path, PathBuf},
    process::Command,
};

fn checked(command: &mut Command) {
    assert!(command.status().unwrap().success(), "{command:?}");
}
struct Mount(PathBuf);
impl Drop for Mount {
    fn drop(&mut self) {
        checked(Command::new("umount").arg(&self.0));
    }
}

fn enable(file: &File) {
    #[repr(C)]
    struct Enable {
        version: u32,
        algorithm: u32,
        block_size: u32,
        salt_size: u32,
        salt: u64,
        signature_size: u32,
        reserved: u32,
        signature: u64,
        reserved_tail: [u64; 11],
    }
    let arg = Enable {
        version: 1,
        algorithm: 1,
        block_size: 4096,
        salt_size: 0,
        salt: 0,
        signature_size: 0,
        reserved: 0,
        signature: 0,
        reserved_tail: [0; 11],
    };
    assert_eq!(
        unsafe { libc::ioctl(file.as_raw_fd(), 0x40806685 as libc::c_ulong, &arg) },
        0,
        "enable verity: {}",
        std::io::Error::last_os_error()
    );
}
fn key(data: &[u8]) -> Key {
    Key::new(Type::Blob, data.len() as u64, data)
}
fn segment(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id:016x}.seg"))
}

fn exercise(base: &Path) {
    let dir = base.join("store");
    let data: Vec<Vec<u8>> = (0..32)
        .map(|i| format!("verified index object {i}").into_bytes())
        .collect();
    let store = Store::open(&dir).unwrap();
    for bytes in &data {
        store.put(key(bytes), bytes).unwrap();
    }
    let snapshot = store.seal_snapshot().unwrap();
    assert!(
        snapshot.validated_index_digests().is_err(),
        "mutable snapshot accepted"
    );
    for (_, file) in snapshot.files() {
        enable(file);
    }
    let proofs = snapshot.validated_index_digests().unwrap();
    assert_eq!(proofs.len(), 1);
    let (&id, proof) = proofs.first_key_value().unwrap();
    let restored = proofs
        .iter()
        .map(|(&id, proof)| {
            (
                id,
                ValidatedIndexDigest::from_authenticated_bytes(*proof.as_bytes()),
            )
        })
        .collect();
    store.close().unwrap();
    drop(store);
    drop(snapshot);
    let mut wrong = proofs.clone();
    let mut bytes = *proof.as_bytes();
    bytes[0] ^= 1;
    wrong.insert(id, ValidatedIndexDigest::from_authenticated_bytes(bytes));
    assert!(Store::open_with_validated_indexes(&dir, Options::default(), &wrong).is_err());
    for verified in [false, true] {
        let store = if verified {
            Store::open_with_validated_indexes(&dir, Options::default(), &restored).unwrap()
        } else {
            Store::open(&dir).unwrap()
        };
        for bytes in &data {
            assert_eq!(store.get(key(bytes)).unwrap(), *bytes);
            assert!(store.has(key(bytes)).unwrap());
        }
        assert!(store.get(key(b"missing")).is_err());
        assert!(!store.has(key(b"missing")).unwrap());
        store.verify(|| false).unwrap();
        store.close().unwrap();
    }
    // A later, unlisted segment must retain ordinary footer validation.
    let store = Store::open_with_validated_indexes(&dir, Options::default(), &proofs).unwrap();
    store.put(key(b"later"), b"later").unwrap();
    let later = store.seal_snapshot().unwrap();
    let later_id = later.files().last().unwrap().0;
    assert_ne!(later_id, id);
    store.close().unwrap();
    drop(store);
    drop(later);
    let store = Store::open_with_validated_indexes(&dir, Options::default(), &proofs).unwrap();
    assert_eq!(store.get(key(b"later")).unwrap(), b"later");
    store.close().unwrap();
    drop(store);
    // Identical bytes without kernel immutability cannot reuse the proof.
    let original = segment(&dir, id);
    let retained = dir.join("retained-original");
    fs::rename(&original, &retained).unwrap();
    fs::copy(&retained, &original).unwrap();
    assert!(Store::open_with_validated_indexes(&dir, Options::default(), &proofs).is_err());
    let normal = Store::open(&dir).unwrap();
    normal.close().unwrap();
    drop(normal);
    fs::remove_file(&original).unwrap();
    // A different immutable segment at the same path cannot reuse the proof.
    fs::copy(segment(&dir, later_id), &original).unwrap();
    enable(&File::open(&original).unwrap());
    assert!(Store::open_with_validated_indexes(&dir, Options::default(), &proofs).is_err());
    fs::remove_file(&original).unwrap();
    fs::rename(&retained, &original).unwrap();
    // Validation must run after sealing, despite an earlier successful parse.
    let bad_dir = base.join("corrupted-before-sealing");
    let bad = Store::open(&bad_dir).unwrap();
    bad.put(key(b"bad"), b"bad").unwrap();
    let captured = bad.seal_snapshot().unwrap();
    let (bad_id, file) = captured.files().next().unwrap();
    let writer = OpenOptions::new()
        .write(true)
        .open(segment(&bad_dir, bad_id))
        .unwrap();
    let offset = file.metadata().unwrap().len() - 16;
    let mut crc = [0; 1];
    file.read_exact_at(&mut crc, offset).unwrap();
    crc[0] ^= 1;
    writer.write_all_at(&crc, offset).unwrap();
    writer.sync_all().unwrap();
    drop(writer);
    enable(file);
    assert!(
        captured.validated_index_digests().is_err(),
        "pre-sealing corruption was certified"
    );
    bad.close().unwrap();
    drop(bad);
    drop(captured);
    assert!(Store::open(&bad_dir).is_err());
    println!(
        "PASS valid proof, digest round-trip, wrong digest, lookup parity, missing keys, full verification, unlisted segment, mutable copy, immutable replacement, pre-sealing corruption"
    );
}
fn main() {
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("new absolute output directory"),
    );
    assert!(directory.is_absolute());
    fs::create_dir(&directory).unwrap();
    let image = directory.join("filesystem.img");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&image)
        .unwrap();
    file.set_len(256 * 1024 * 1024).unwrap();
    drop(file);
    checked(
        Command::new("mkfs.ext4")
            .args(["-q", "-F", "-b", "4096", "-O", "verity"])
            .arg(&image),
    );
    let mount = directory.join("mount");
    fs::create_dir(&mount).unwrap();
    checked(
        Command::new("mount")
            .args(["-o", "loop,nodev,nosuid,noexec"])
            .arg(&image)
            .arg(&mount),
    );
    let mounted = Mount(mount);
    exercise(&mounted.0);
    drop(mounted);
    fs::write(
        directory.join("result.txt"),
        "PASS: immutable index validation and rejection checks; image unmounted\n",
    )
    .unwrap();
}
