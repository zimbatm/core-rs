use amber_store_core::packstore::{SegmentLiveness, Store};
use serde_json::{Value, json};
use std::{cell::RefCell, error::Error, fs, path::Path, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn usage() -> std::io::Result<libc::rusage> {
    let mut value = std::mem::MaybeUninit::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, value.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // getrusage initializes the structure on success.
    Ok(unsafe { value.assume_init() })
}

fn seconds(value: libc::timeval) -> f64 {
    value.tv_sec as f64 + value.tv_usec as f64 / 1e6
}

fn save(directory: &Path, name: &str, value: &Value) -> Result<()> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(name))?;
    serde_json::to_writer_pretty(file, value)?;
    Ok(())
}

fn rows(report: &[SegmentLiveness]) -> Value {
    json!(
        report
            .iter()
            .map(|s| json!({
                "id":s.id,"sealed":s.sealed,"live_keys":s.live_keys,"dead_keys":s.dead_keys,
                "live_bytes":s.live_bytes,"dead_bytes":s.dead_bytes
            }))
            .collect::<Vec<_>>()
    )
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err(
            "usage: indexed-liveness-profile STORE NEW_OUTPUT_DIRECTORY ROUNDS KEY_BYTE".into(),
        );
    }
    let source = Path::new(&args[1]);
    let directory = Path::new(&args[2]);
    let rounds: usize = args[3].parse()?;
    let selection_byte: usize = args[4].parse()?;
    if ![30, 31].contains(&selection_byte) {
        return Err("KEY_BYTE must be 30 (mixed) or 31 (index buckets)".into());
    }
    if !source.is_absolute() || !directory.is_absolute() || !(1..=3).contains(&rounds) {
        return Err("expected absolute paths and 1..3 rounds".into());
    }
    fs::create_dir(directory)?;
    let opening = Instant::now();
    let store = Store::open(source)?;
    let open_seconds = opening.elapsed().as_secs_f64();
    let mut cases = Vec::new();
    for threshold in [0u16, 26, 128, 230, 256] {
        let preparation = Instant::now();
        let marks = RefCell::new(store.new_mark_set()?);
        if threshold != 0 {
            store.liveness(|key| {
                if u16::from(key.as_bytes()[selection_byte]) < threshold {
                    marks.borrow_mut().mark(key);
                }
                false
            })?;
        }
        let marks = marks.into_inner();
        let preparation_seconds = preparation.elapsed().as_secs_f64();
        let prime = Instant::now();
        let expected = store.liveness(|key| marks.contains(key))?;
        let prime_seconds = prime.elapsed().as_secs_f64();
        save(
            directory,
            &format!("{threshold}-expected.json"),
            &rows(&expected),
        )?;
        let mut samples = Vec::new();
        for pair in 0..rounds {
            let modes = if pair % 2 == 0 {
                ["key", "indexed"]
            } else {
                ["indexed", "key"]
            };
            for mode in modes {
                let before = usage()?;
                let started = Instant::now();
                let actual = if mode == "key" {
                    store.liveness(|key| marks.contains(key))?
                } else {
                    store.liveness_marked(&marks)?
                };
                let elapsed = started.elapsed().as_secs_f64();
                let after = usage()?;
                if actual != expected {
                    save(
                        directory,
                        &format!("{threshold}-{pair}-{mode}-mismatch.json"),
                        &rows(&actual),
                    )?;
                    return Err("liveness results changed".into());
                }
                let sample = json!({
                    "threshold":threshold,"pair":pair,"mode":mode,"seconds":elapsed,
                    "user_seconds":seconds(after.ru_utime)-seconds(before.ru_utime),
                    "system_seconds":seconds(after.ru_stime)-seconds(before.ru_stime)
                });
                save(
                    directory,
                    &format!("{threshold}-{pair}-{mode}.json"),
                    &sample,
                )?;
                fs::write(
                    directory.join("progress.json"),
                    serde_json::to_vec(&sample)?,
                )?;
                println!("{sample}");
                samples.push(sample);
            }
        }
        let case = json!({
            "threshold":threshold,"marked_keys":marks.marked(),
            "live_records":expected.iter().map(|s| s.live_keys).sum::<usize>(),
            "dead_records":expected.iter().map(|s| s.dead_keys).sum::<usize>(),
            "preparation_seconds":preparation_seconds,"prime_seconds":prime_seconds,
            "samples":samples
        });
        save(directory, &format!("{threshold}-result.json"), &case)?;
        cases.push(case);
    }
    store.close()?;
    save(
        directory,
        "result.json",
        &json!({
            "correctness":"passed","source":source,"rounds":rounds,"selection_byte":selection_byte,"open_seconds":open_seconds,
            "cases":cases,
            "method":"Read-only record classification in one process. Thresholds on key byte 31 group by index bucket; byte 30 mixes membership within buckets. These select synthetic live fractions, not closed repository roots. One key-based warmup precedes alternating pairs for each captured mark set. Every per-segment result must match. Timers exclude opening, mark preparation, result validation, and serialization. No ingest, publication, or collection. Normal store opening and closing can recover or seal existing data."
        }),
    )?;
    Ok(())
}
