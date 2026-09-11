use amber_store_core::{
    fstree::{check_complete, child_keys},
    key::{Key, Type},
    packstore::Store,
};
use serde_json::json;
use std::{
    fs,
    io::{self, BufWriter, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Instant,
};

fn read(store: &Store, key: Key, reads: &AtomicU64) -> io::Result<Vec<u8>> {
    let data = store.get(key).map_err(io::Error::other)?;
    if Key::new(key.type_(), key.length(), &data) != key {
        return Err(io::Error::other("interior checksum mismatch"));
    }
    reads.fetch_add(1, Relaxed);
    Ok(data)
}

fn usage() -> io::Result<libc::rusage> {
    let mut value = std::mem::MaybeUninit::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // getrusage initializes the structure on success.
    Ok(unsafe { value.assume_init() })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err(
            "usage: closure-membership-profile STORE ROOT NEW_OUTPUT_DIRECTORY MODE".into(),
        );
    }
    let source = Path::new(&args[1]);
    let output = Path::new(&args[3]);
    let mode = args[4].as_str();
    if !source.is_absolute()
        || !output.is_absolute()
        || !matches!(mode, "list" | "indexed" | "indexed-parallel")
    {
        return Err("expected absolute paths and list, indexed, or indexed-parallel mode".into());
    }
    if args[2].len() != 64 || !args[2].is_ascii() {
        return Err("expected 64 hexadecimal root digits".into());
    }
    let bytes = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&args[2][i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>()?;
    let root = Key::parse(&bytes)?;
    fs::create_dir(output)?;
    let opening = Instant::now();
    let store = Store::open(source)?;
    let open_seconds = opening.elapsed().as_secs_f64();
    let reads = AtomicU64::new(0);
    let before = usage()?;
    let started = Instant::now();
    let mut marks = store.new_mark_set()?;
    let snapshot_seconds = started.elapsed().as_secs_f64();
    let mut duplicate_pops = 0u64;
    let mut peak_pending = 0usize;
    let mut keys_buffer_bytes = 0usize;
    let walk_started = Instant::now();
    let membership_seconds;
    if mode == "list" {
        let keys = check_complete(
            root,
            |key| read(&store, key, &reads),
            |key| store.has(key).map_err(io::Error::other),
            8,
        )?;
        keys_buffer_bytes = keys.capacity() * std::mem::size_of::<Key>();
        let membership = Instant::now();
        for key in &keys {
            if !marks.mark(*key).1 {
                return Err("validated key missing from captured membership".into());
            }
        }
        membership_seconds = membership.elapsed().as_secs_f64();
    } else if mode == "indexed-parallel" {
        let mut pending = vec![root];
        let mut batch = Vec::with_capacity(4096);
        peak_pending = 1;
        while !pending.is_empty() {
            batch.clear();
            while batch.len() < 4096 {
                let Some(key) = pending.pop() else { break };
                let (newly, present) = marks.mark(key);
                if !present {
                    return Err("reachable key missing from captured membership".into());
                }
                if !newly {
                    duplicate_pops += 1;
                    continue;
                }
                if !matches!(key.type_(), Type::Blob | Type::XattrSet) {
                    batch.push(key);
                }
            }
            if batch.is_empty() {
                continue;
            }
            std::thread::scope(|scope| -> io::Result<()> {
                let workers: Vec<_> = batch
                    .chunks(batch.len().div_ceil(8))
                    .map(|keys| {
                        let store = &store;
                        let reads = &reads;
                        scope.spawn(move || -> io::Result<Vec<Key>> {
                            let mut children = Vec::new();
                            for &key in keys {
                                let data = read(store, key, reads)?;
                                children.extend(child_keys(key, &data).map_err(io::Error::other)?);
                            }
                            Ok(children)
                        })
                    })
                    .collect();
                for worker in workers {
                    pending.extend(
                        worker
                            .join()
                            .map_err(|_| io::Error::other("read worker panicked"))??,
                    );
                    peak_pending = peak_pending.max(pending.len());
                }
                Ok(())
            })?;
        }
        membership_seconds = 0.0;
    } else {
        let mut pending = vec![root];
        peak_pending = 1;
        while let Some(key) = pending.pop() {
            let (newly, present) = marks.mark(key);
            if !present {
                return Err("reachable key missing from captured membership".into());
            }
            if !newly {
                duplicate_pops += 1;
                continue;
            }
            if !matches!(key.type_(), Type::Blob | Type::XattrSet) {
                let data = read(&store, key, &reads)?;
                pending.extend(child_keys(key, &data)?);
                peak_pending = peak_pending.max(pending.len());
            }
        }
        membership_seconds = 0.0;
    }
    let walk_and_membership_seconds = walk_started.elapsed().as_secs_f64();
    let marked = marks.marked();

    let total_seconds = started.elapsed().as_secs_f64();
    let after = usage()?;
    let verification = Instant::now();
    let selected = std::cell::RefCell::new(Vec::with_capacity(marked));
    store.liveness(|key| {
        if marks.contains(key) {
            selected.borrow_mut().push(key);
        }
        false
    })?;
    let mut selected = selected.into_inner();
    selected.sort_unstable();
    selected.dedup();
    if selected.len() != marked {
        return Err("membership enumeration changed".into());
    }
    let mut manifest = BufWriter::new(fs::File::create(output.join("membership.bin"))?);
    let mut digest = blake3::Hasher::new();
    for chunk in selected.chunks(1024) {
        let mut chunk_digest = blake3::Hasher::new();
        for key in chunk {
            digest.update(key.as_bytes());
            chunk_digest.update(key.as_bytes());
        }
        manifest.write_all(&(chunk.len() as u64).to_le_bytes())?;
        manifest.write_all(chunk_digest.finalize().as_bytes())?;
    }
    manifest.flush()?;
    let verification_seconds = verification.elapsed().as_secs_f64();
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    let result = json!({
        "mode": mode, "root": args[2], "objects": marked, "interior_reads": reads.load(Relaxed),
        "membership_digest": digest.finalize().to_hex().to_string(),
        "verification_seconds": verification_seconds, "open_seconds": open_seconds,
        "workers": if mode == "indexed" { 1 } else { 8 }, "interior_batch_limit": if mode == "indexed-parallel" { Some(4096) } else { None::<usize> }, "snapshot_seconds": snapshot_seconds, "walk_and_membership_seconds": walk_and_membership_seconds,
        "membership_seconds": membership_seconds, "total_seconds": total_seconds,
        "user_seconds": seconds(after.ru_utime)-seconds(before.ru_utime),
        "system_seconds": seconds(after.ru_stime)-seconds(before.ru_stime),
        "peak_rss_bytes": after.ru_maxrss as u64 * 1024,
        "keys_buffer_bytes": keys_buffer_bytes, "duplicate_pops": duplicate_pops,
        "peak_pending": peak_pending,
        "scope": "Initial closure and membership only; list uses 8 workers, indexed uses serial DFS; indexed-parallel batches at most 4096 interiors across 8 workers. No boundaries, sealing, fs-verity, signing, Git validation, or publication. RSS covers opening and the timed phase. Sorted-key hash-manifest verification follows outside timing and RSS sampling."
    });
    serde_json::to_writer_pretty(fs::File::create(output.join("result.json"))?, &result)?;
    println!("{result}");
    store.close()?;
    Ok(())
}
