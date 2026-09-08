use crc_fast::{CrcAlgorithm::Crc32Iscsi, Digest};
use serde_json::json;
use std::{fs, hint::black_box, path::PathBuf, time::Instant};

fn candidate(bytes: &[u8]) -> u32 {
    crc_fast::crc32_iscsi(bytes)
}

fn record_baseline(bytes: &[u8]) -> u32 {
    let crc = crc32c::crc32c(black_box(&[42; 42]));
    let crc = crc32c::crc32c_append(crc, &[0; 4]);
    crc32c::crc32c_append(crc, bytes)
}

fn record_candidate(bytes: &[u8]) -> u32 {
    let mut digest = Digest::new(Crc32Iscsi);
    digest.update(black_box(&[42; 42]));
    digest.update(&[0; 4]);
    digest.update(bytes);
    digest.finalize() as u32
}

fn payload(size: usize) -> Vec<u8> {
    let mut state = 0x6a09e667f3bcc909u64;
    (0..size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn compatible() {
    assert_eq!(candidate(b"123456789"), 0xe3069283);
    let data = payload(4096 + 64);
    for size in 0..=4096 {
        for offset in [0, 1, 7, 31, 63] {
            let bytes = &data[offset..offset + size];
            let expected = crc32c::crc32c(bytes);
            assert_eq!(candidate(bytes), expected, "size={size} offset={offset}");
            for split in [0, 1.min(size), size / 2, size] {
                let mut digest = Digest::new(Crc32Iscsi);
                digest.update(&bytes[..split]);
                digest.update(&bytes[split..]);
                assert_eq!(digest.finalize(), u64::from(expected));
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(3..=4).contains(&args.len()) {
        return Err(
            "usage: checksum-throughput NEW_ABSOLUTE_DIRECTORY MIB_PER_ROUND [whole|record]".into(),
        );
    }
    let operation = args.get(3).map(String::as_str).unwrap_or("whole");
    let (baseline, candidate): (fn(&[u8]) -> u32, fn(&[u8]) -> u32) = match operation {
        "whole" => (crc32c::crc32c, candidate),
        "record" => (record_baseline, record_candidate),
        _ => return Err("operation must be whole or record".into()),
    };
    let directory = PathBuf::from(&args[1]);
    let work = args[2]
        .parse::<usize>()?
        .checked_mul(1024 * 1024)
        .ok_or("work size overflow")?;
    if !directory.is_absolute() || work == 0 {
        return Err("require an absolute new directory and positive work size".into());
    }
    fs::create_dir(&directory)?;
    compatible();
    let offsets = [0usize, 1, 7, 31, 63];
    let mut measurements = Vec::new();
    for size in [
        0,
        16,
        64,
        256,
        4096,
        65536,
        1024 * 1024,
        64 * 1024 * 1024,
        512 * 1024 * 1024,
    ] {
        let data = payload(size + 64);
        let expected: Vec<_> = offsets
            .iter()
            .map(|&offset| {
                let bytes = &data[offset..offset + size];
                let expected = baseline(bytes);
                assert_eq!(candidate(bytes), expected);
                expected
            })
            .collect();
        let checksum_bytes = size + if operation == "record" { 46 } else { 0 };
        let iterations = (work / checksum_bytes.max(16)).max(1);
        let expected_sum = expected
            .iter()
            .copied()
            .fold(0u32, u32::wrapping_add)
            .wrapping_mul((iterations / offsets.len()) as u32)
            .wrapping_add(
                expected[..iterations % offsets.len()]
                    .iter()
                    .copied()
                    .fold(0u32, u32::wrapping_add),
            );
        for round in 0..3 {
            for fast in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let checksum: fn(&[u8]) -> u32 = if fast { candidate } else { baseline };
                let start = Instant::now();
                let mut sum = 0u32;
                for index in 0..iterations {
                    let offset = offsets[index % offsets.len()];
                    sum = sum
                        .wrapping_add(black_box(checksum(black_box(&data[offset..offset + size]))));
                }
                let seconds = start.elapsed().as_secs_f64();
                assert_eq!(sum, expected_sum);
                measurements.push(json!({
                    "engine":if fast {"crc-fast"} else {"crc32c"},
                    "round":round,"bytes":size,"iterations":iterations,
                    "processed_bytes":checksum_bytes as u64 * iterations as u64,
                    "seconds":seconds,"checksum_sum":sum
                }));
            }
        }
    }
    let report = json!({
        "schema":2,"operation":operation,"correctness":"passed","algorithm":"CRC-32/ISCSI",
        "candidate_version":"1.10.0","candidate_target":crc_fast::get_calculator_target(Crc32Iscsi),
        "architecture":std::env::consts::ARCH,
        "method":"Synthetic deterministic random bytes. Three alternating rounds per size, five alignment offsets, warm process and memory. Function calls and checksum accumulation are timed. Input generation, exact checksum comparisons, known-vector validation, and split-update compatibility checks are outside timing. This is checksum throughput, not a complete storage workload.",
        "measurements":measurements
    });
    fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
