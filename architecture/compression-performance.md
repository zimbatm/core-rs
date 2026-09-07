# Compression context reuse

Record encoding retains one Zstandard bulk compressor per writer thread.
Independent threads do not share a compression lock.
Each record remains an independent frame with the same compression level and format.
Compression failure retains the existing raw-record behavior.
The workspace remains allocated until its thread exits.

The synthetic benchmark compares fresh and reused contexts.
It validates identical frames and decompression before measuring compression.
It uses 32 varying records at each size and three rounds with alternating mode order.
The measurement excludes input generation, verification, and filesystem I/O.

Run from a development shell:

```
cargo run --release --example compression-throughput -- /absolute/new/results 10000
```

The initial agents-host run used 10,000 records per round.
Median times were:

| Record size | Fresh context | Reused context |
| --- | ---: | ---: |
| 256 bytes | 0.065117 s | 0.034182 s |
| 4 KiB | 0.114815 s | 0.077237 s |
| 64 KiB | 0.686361 s | 0.639111 s |

These are compression measurements, not store or import throughput.
Large-record samples varied, including one slower reused-context round.
The full workload benefit remains unmeasured.

All 424 unit tests and the integration and golden-vector suites passed.
The new test checks independent frames across four threads and changing payload sizes.
