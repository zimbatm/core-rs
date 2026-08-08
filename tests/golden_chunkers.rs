//! Golden-vector tests for the chunkers module (VECTORS.md: `ultracdc.json`
//! and `item_chunker.json`).

mod common;

use amber_store_core::chunkers::{self, ByteOpts, ItemChunker};

#[derive(serde::Deserialize)]
struct UltraFile {
    cases: Vec<UltraCase>,
}

#[derive(serde::Deserialize)]
struct UltraCase {
    name: String,
    min: usize,
    normal: usize,
    max: usize,
    data: common::Payload,
    chunks: Vec<usize>,
}

#[test]
fn golden_ultracdc() {
    let Some(raw) = common::load("ultracdc.json") else {
        return;
    };
    let file: UltraFile = serde_json::from_slice(&raw).expect("parse ultracdc.json");
    assert!(!file.cases.is_empty(), "ultracdc.json has no cases");
    for case in &file.cases {
        // Zero min/normal/max select the library defaults, both in the
        // vectors and in ByteOpts.
        let opts = ByteOpts {
            min_size: case.min,
            max_size: case.max,
            normal_size: case.normal,
            ..ByteOpts::default()
        };
        let input = case.data.bytes();
        let mut lens = Vec::new();
        let mut reassembled = Vec::new();
        chunkers::split_bytes(input.as_slice(), Some(&opts), |chunk| {
            lens.push(chunk.len());
            reassembled.extend_from_slice(&chunk);
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap_or_else(|e| panic!("case {:?}: split_bytes: {e}", case.name));
        assert_eq!(lens, case.chunks, "case {:?}: chunk lengths", case.name);
        assert_eq!(
            lens.iter().sum::<usize>(),
            input.len(),
            "case {:?}: chunk lengths must sum to the input length",
            case.name
        );
        assert_eq!(reassembled, input, "case {:?}: reassembly", case.name);
    }
}

#[derive(serde::Deserialize)]
struct ItemFile {
    cases: Vec<ItemCase>,
}

#[derive(serde::Deserialize)]
struct ItemCase {
    bits: u32,
    items: Vec<common::Payload>,
    runs: Vec<usize>,
}

#[test]
fn golden_item_chunker() {
    let Some(raw) = common::load("item_chunker.json") else {
        return;
    };
    let file: ItemFile = serde_json::from_slice(&raw).expect("parse item_chunker.json");
    assert!(!file.cases.is_empty(), "item_chunker.json has no cases");
    for (i, case) in file.cases.iter().enumerate() {
        let chunker = ItemChunker::new(case.bits);
        // run_len counts items in the current run including the current one;
        // a true IsBoundary closes the run. The final (possibly unterminated)
        // run is included.
        let mut runs = Vec::new();
        let mut run_len = 0usize;
        for item in &case.items {
            let enc = item.bytes();
            run_len += 1;
            if chunker.is_boundary(&enc, run_len) {
                runs.push(run_len);
                run_len = 0;
            }
        }
        if run_len > 0 {
            runs.push(run_len);
        }
        assert_eq!(runs, case.runs, "case {i} (bits={})", case.bits);
        assert_eq!(
            runs.iter().sum::<usize>(),
            case.items.len(),
            "case {i} (bits={}): runs must sum to the item count",
            case.bits
        );
    }
}
