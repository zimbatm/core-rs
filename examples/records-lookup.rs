use amber_store_core::{fstree, key::Key, packstore::Store};
use serde_json::json;
use std::{fs, path::PathBuf, time::Instant};

fn ordered(mut keys: Vec<Key>) -> Vec<Key> {
    keys.sort_unstable_by(|a, b| {
        a.as_bytes()[31]
            .cmp(&b.as_bytes()[31])
            .then_with(|| a.as_bytes().cmp(b.as_bytes()))
    });
    keys
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err(
            "usage: records-lookup SOURCE_STORE VIEW_STORE ROOT NEW_OUTPUT_DIRECTORY".into(),
        );
    }
    let output = PathBuf::from(&args[4]);
    if !output.is_absolute() {
        return Err("output directory must be absolute".into());
    }
    fs::create_dir(&output)?;
    let source = Store::open(&args[1])?;
    let view = Store::open(&args[2])?;
    let root = Key::parse(&hex_decode(&args[3])?)?;
    let keys = fstree::reachable_keys(root, |key| view.get(key))?;
    let mut samples = Vec::new();
    for round in 0..7 {
        for sorted in if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let started = Instant::now();
            let input = keys.clone();
            let records = source.records_in_order(if sorted { ordered(input) } else { input })?;
            let seconds = started.elapsed().as_secs_f64();
            assert_eq!(records.len(), keys.len());
            std::hint::black_box(&records);
            samples.push(json!({"round":round,"sorted":sorted,"seconds":seconds}));
        }
    }
    let mut baseline = source.records_in_order(keys.clone())?;
    let mut candidate = source.records_in_order(ordered(keys.clone()))?;
    let mut verified = 0;
    loop {
        match (baseline.next(), candidate.next()) {
            (None, None) => break,
            (Some(a), Some(b)) => {
                assert_eq!(a?, b?);
                verified += 1;
            }
            _ => return Err("record count mismatch".into()),
        }
    }
    assert_eq!(verified, keys.len());
    source.close()?;
    view.close()?;
    let report = json!({"objects":keys.len(),"encoded_records_identical":true,"samples":samples,
        "scope":"Warm-host record lookup only. Includes input cloning and candidate sorting. Excludes store opening, reachability, record reads, and copying."});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn hex_decode(text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if text.len() != 64 || !text.is_ascii() {
        return Err("root must contain 64 hexadecimal characters".into());
    }
    (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(Into::into))
        .collect()
}
