use super::{CopyBatch, Error, Store, ValidatedRecord};
use crate::amberpack::encode_record;
use crate::packstore::{Options, testutil::blob_obj};
use serde_json::json;
use std::{borrow::Cow, collections::HashSet, fs, path::PathBuf, time::Instant};

const KEYS_PER_SEGMENT: usize = 8192;
const QUERIES: usize = 32768;
const BATCH_SIZE: usize = 256;
const ROUNDS: usize = 6;

struct Queries {
    records: Vec<Result<ValidatedRecord<'static>, Error>>,
    expected: Vec<bool>,
}

impl Queries {
    fn new(stored: usize, mode: &str) -> Self {
        let mut records = Vec::with_capacity(QUERIES);
        let mut expected = Vec::with_capacity(QUERIES);
        let mut seen = HashSet::new();
        for index in 0..QUERIES {
            if index % BATCH_SIZE == 0 {
                seen.clear();
            }
            let id = match mode {
                "absent" => stored + index,
                "present" => (index * 104729) % stored,
                "mixed" if index % 2 == 0 => (index * 104729) % stored,
                "mixed" => stored + index,
                "duplicates" => stored + index / 2,
                _ => unreachable!(),
            };
            let object = blob_obj(&(id as u64).to_le_bytes());
            expected.push(id < stored || !seen.insert(object.key));
            let bytes = encode_record(object.key, &object.data).unwrap();
            records.push(ValidatedRecord::new(object.key, Cow::Owned(bytes)));
        }
        Self { records, expected }
    }
}

fn schedstat() -> Vec<u64> {
    fs::read_to_string("/proc/thread-self/schedstat")
        .unwrap()
        .split_whitespace()
        .map(|value| value.parse().unwrap())
        .collect()
}

fn measure(
    store: &Store,
    queries: &Queries,
    batch: &mut CopyBatch,
    method: &str,
) -> serde_json::Value {
    let mut observed = Vec::with_capacity(queries.records.len());
    let before = schedstat();
    let started = Instant::now();
    for records in queries.records.chunks(BATCH_SIZE) {
        let _append = store.append_lock();
        match method {
            "scalar" => {
                batch.present.clear();
                batch.seen.clear();
                for record in records {
                    let record = record.as_ref().unwrap();
                    let present = !batch.seen.insert(record.key)
                        || store.has(std::hint::black_box(record.key)).unwrap();
                    batch.present.push(present);
                }
            }
            "batch" => store
                .copy_membership(std::hint::black_box(records), batch)
                .unwrap(),
            _ => unreachable!(),
        }
        observed.extend_from_slice(std::hint::black_box(&batch.present));
    }
    let seconds = started.elapsed().as_secs_f64();
    let after = schedstat();
    assert_eq!(observed, queries.expected, "membership result differs");
    json!({
        "method":method,
        "seconds":seconds,
        "thread_cpu_seconds":(after[0] - before[0]) as f64 / 1e9,
        "thread_runqueue_seconds":(after[1] - before[1]) as f64 / 1e9,
        "scheduler_timeslices":after[2] - before[2],
        "correctness":"passed",
    })
}

#[test]
#[ignore = "Run the membership-bench package with AMBER_BENCH_DIR set to a new absolute directory"]
fn membership_benchmark() {
    let keys_per_segment = std::env::var("AMBER_BENCH_KEYS")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(KEYS_PER_SEGMENT);
    assert!((1..=65536).contains(&keys_per_segment));
    let directory = PathBuf::from(std::env::var("AMBER_BENCH_DIR").unwrap());
    assert!(
        directory.is_absolute(),
        "benchmark directory must be absolute"
    );
    fs::create_dir(&directory).unwrap();
    let store = Store::open_with(
        directory.join("store"),
        Options::default().segment_size(u64::MAX),
    )
    .unwrap();
    let mut samples = Vec::new();
    let setup_started = Instant::now();
    let mut measurement_seconds = 0.0;
    for segment in 0..256 {
        {
            let mut append = store.append_lock();
            for offset in 0..keys_per_segment {
                let id = segment * keys_per_segment + offset;
                let object = blob_obj(&(id as u64).to_le_bytes());
                let bytes = encode_record(object.key, &object.data).unwrap();
                store
                    .append_locked(&mut append, object.key, &bytes, false)
                    .unwrap();
            }
            store.seal_active(&mut append).unwrap();
        }
        let segments = segment + 1;
        if ![8, 64, 256].contains(&segments) {
            continue;
        }
        assert_eq!(store.segments().unwrap().len(), segments);
        for mode in ["absent", "present", "mixed", "duplicates"] {
            let queries = Queries::new(segments * keys_per_segment, mode);
            let mut batch = CopyBatch::default();
            let measurement_started = Instant::now();
            for method in ["scalar", "batch"] {
                measure(&store, &queries, &mut batch, method);
            }
            for round in 0..ROUNDS {
                let methods = if round % 2 == 0 {
                    ["scalar", "batch"]
                } else {
                    ["batch", "scalar"]
                };
                for (position, method) in methods.into_iter().enumerate() {
                    let pressure_before = fs::read_to_string("/proc/pressure/cpu").unwrap();
                    let mut sample = measure(&store, &queries, &mut batch, method);
                    sample["segments"] = json!(segments);
                    sample["mode"] = json!(mode);
                    sample["round"] = json!(round);
                    sample["position"] = json!(position);
                    sample["cpu_pressure_before"] = json!(pressure_before);
                    sample["cpu_pressure_after"] =
                        json!(fs::read_to_string("/proc/pressure/cpu").unwrap());
                    println!("{sample}");
                    samples.push(sample);
                }
            }
            measurement_seconds += measurement_started.elapsed().as_secs_f64();
        }
    }
    store.close().unwrap();
    let report = json!({
        "fixture":"unique eight-byte blob payloads; sequential object IDs; valid encoded records",
        "segments":[8,64,256],
        "keys_per_segment":keys_per_segment,
        "queries_per_sample":QUERIES,
        "batch_size":BATCH_SIZE,
        "rounds":ROUNDS,
        "seed":"deterministic sequential IDs with multiplier 104729 for present queries",
        "elapsed_seconds":setup_started.elapsed().as_secs_f64(),
        "measurement_block_seconds":measurement_seconds,
        "scope":"Warm synthetic membership only. Includes append locking and result collection. Excludes fixture creation, source validation, writes, rotation, sync, and result assertions. Scalar uses Store::has and the same planned-input dedup set. Batch calls the candidate copy_membership directly. This is not complete restore performance.",
        "thread_cpu_scope":"Delta of /proc/thread-self/schedstat; sampling reads slightly extend the wall-timed interval.",
        "samples":samples,
    });
    fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
