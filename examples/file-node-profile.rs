use amber_store_core::{
    fstree::decode_file_node,
    key::{Key, Type},
    packstore::Store,
};
use serde_json::json;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::RefCell,
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Instant,
};

struct CountAlloc;
#[global_allocator]
static ALLOCATOR: CountAlloc = CountAlloc;
static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

// System retains ownership and alignment rules; counters cannot allocate.
unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            CALLS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size() as u64, Relaxed);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            CALLS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size() as u64, Relaxed);
        }
        p
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(p, layout, size) };
        if !p.is_null() {
            CALLS.fetch_add(1, Relaxed);
            BYTES.fetch_add(size as u64, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: file-node-profile STORE NEW_OUTPUT_DIRECTORY".into());
    }
    let source = Path::new(&args[1]);
    let output = Path::new(&args[2]);
    if !source.is_absolute() || !output.is_absolute() {
        return Err("expected absolute paths".into());
    }
    fs::create_dir(output)?;
    let store = Store::open(source)?;
    let selected = RefCell::new(Vec::new());
    store.liveness(|key| {
        let mut keys = selected.borrow_mut();
        if key.type_() == Type::FileNode && keys.len() < 4096 && !keys.contains(&key) {
            keys.push(key);
        }
        false
    })?;
    let mut inputs = Vec::new();
    let mut manifest = Vec::new();
    let mut digest = blake3::Hasher::new();
    for key in selected.into_inner() {
        let data = store.get(key)?;
        if Key::new(key.type_(), key.length(), &data) != key {
            return Err("object checksum mismatch".into());
        }
        let expected = decode_file_node(&data)?;
        digest.update(key.as_bytes());
        for child in &expected {
            digest.update(child.as_bytes());
        }
        manifest.push(json!({"key":key.to_string(),"bytes":data.len(),"children":expected.len()}));
        inputs.push((data, expected));
    }
    store.close()?;
    if inputs.is_empty() {
        return Err("source has no file nodes".into());
    }
    let mut samples = Vec::new();
    for round in 0..4 {
        let calls = CALLS.load(Relaxed);
        let bytes = BYTES.load(Relaxed);
        let started = Instant::now();
        for _ in 0..10 {
            for (data, expected) in &inputs {
                let decoded = decode_file_node(data)?;
                assert_eq!(&decoded, expected);
                std::hint::black_box(decoded);
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        let calls = CALLS.load(Relaxed) - calls;
        let bytes = BYTES.load(Relaxed) - bytes;
        samples.push(json!({"round":round,"warmup":round==0,"seconds":seconds,"allocation_calls":calls,"requested_bytes":bytes,"decodes":inputs.len()*10}));
    }
    let result = json!({"source":source,"correctness":"passed","manifest_digest":digest.finalize().to_hex().to_string(),"manifest":manifest,"samples":samples,
        "method":"First 4096 distinct FileNode keys in physical segment/footer order. Source object hashes checked. Manifest digest covers parent keys and ordered decoded children. One warmup and three rounds, ten decodes per object per round. Every decoded array compared exactly. Timing includes allocation counters, comparison, and deallocation. Opening, selection, reading, and expected-result construction excluded. Requested bytes include full new realloc sizes, not peak or live memory. This is a decoder sample, not complete GC or uninstrumented performance."});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!(
        "{}",
        serde_json::to_string(&json!({"objects":inputs.len(),"samples":samples}))?
    );
    Ok(())
}
