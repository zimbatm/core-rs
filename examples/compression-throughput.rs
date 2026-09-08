use serde_json::json;
use std::{fs, path::PathBuf, time::Instant};

fn payloads(size: usize) -> Vec<Vec<u8>> {
    (0..32u64)
        .map(|index| {
            let mut state = index + 1;
            (0..size)
                .map(|offset| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    if index % 4 == 0 || offset % 64 < 8 {
                        state as u8
                    } else {
                        b"let package = { name = \"sample\"; version = \"1.0\"; };\n"[offset % 51]
                    }
                })
                .collect()
        })
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(3..=4).contains(&args.len()) {
        return Err(
            "usage: compression-throughput NEW_ABSOLUTE_DIRECTORY RECORDS_PER_ROUND [compress|decompress]".into(),
        );
    }
    let operation = args.get(3).map(String::as_str).unwrap_or("compress");
    if !matches!(operation, "compress" | "decompress") {
        return Err("operation must be compress or decompress".into());
    }
    let directory = PathBuf::from(&args[1]);
    let count: usize = args[2].parse()?;
    if !directory.is_absolute() || count == 0 {
        return Err("require an absolute new directory and positive record count".into());
    }
    fs::create_dir(&directory)?;
    let level = zstd::DEFAULT_COMPRESSION_LEVEL;
    let mut measurements = Vec::new();
    for size in [256, 4096, 65536] {
        let inputs = payloads(size);
        let mut compressor = zstd::bulk::Compressor::new(level)?;
        for data in &inputs {
            let expected = zstd::bulk::compress(data, level)?;
            assert_eq!(compressor.compress(data)?, expected);
            assert_eq!(zstd::bulk::decompress(&expected, data.len())?, *data);
        }
        let encoded: Vec<Vec<u8>> = inputs
            .iter()
            .map(|data| zstd::bulk::compress(data, level))
            .collect::<Result<_, _>>()?;
        let mut decoder = zstd::bulk::Decompressor::new()?;
        for (data, frame) in inputs.iter().zip(&encoded) {
            assert_eq!(decoder.decompress(frame, data.len())?, *data);
        }
        for round in 0..3 {
            let order = if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for reused in order {
                let start = Instant::now();
                let mut compressor = if reused && operation == "compress" {
                    Some(zstd::bulk::Compressor::new(level)?)
                } else {
                    None
                };
                let mut decoder = if reused && operation == "decompress" {
                    Some(zstd::bulk::Decompressor::new()?)
                } else {
                    None
                };
                let mut stored = 0u64;
                for index in 0..count {
                    let data = &inputs[index % inputs.len()];
                    let output = if operation == "compress" {
                        match &mut compressor {
                            Some(compressor) => compressor.compress(data)?,
                            None => zstd::bulk::compress(data, level)?,
                        }
                    } else {
                        let frame = &encoded[index % encoded.len()];
                        match &mut decoder {
                            Some(decoder) => decoder.decompress(frame, data.len())?,
                            None => zstd::bulk::decompress(frame, data.len())?,
                        }
                    };
                    stored += std::hint::black_box(output).len() as u64;
                }
                measurements.push(json!({
                    "mode":if reused {"reused_context"} else {"one_shot"},
                    "round":round,"record_bytes":size,"records":count,
                    "uncompressed_bytes":size as u64 * count as u64,"output_bytes":stored,
                    "seconds":start.elapsed().as_secs_f64()
                }));
            }
        }
    }
    let report = json!({
        "schema":2,"operation":operation,"compression_level":level,"correctness":"identical_frames_and_round_trip",
        "method":"Synthetic corpus: 32 varying records per size, one quarter random bytes, others mixed text and random fields. Three rounds with alternating mode order. Context initialization is timed; input generation and correctness checks are excluded. No filesystem I/O is timed.",
        "measurements":measurements
    });
    fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
