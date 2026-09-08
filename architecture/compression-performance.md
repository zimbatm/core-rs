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

## Decompression context reuse

Record decoding now retains one bulk decompressor per reader thread.
The returned payload remains caller-owned.
The output size cap, exact decoded-length check, and corruption errors remain enforced.
Threads do not share a decoder lock.
The context workspace remains allocated until its thread exits.

The benchmark accepts an optional compress or decompress operation.
Compression remains the default.
Schema 2 reports uncompressed_bytes and output_bytes with an explicit operation.

```text
cargo run --offline --release --example compression-throughput -- /absolute/new/results 10000 decompress
```

The local comparison used 10,000 records per round and three alternating rounds.
It checked decoded byte equality outside timing.

| Record size | Fresh median seconds | Reused median seconds | Fresh / reused |
| --- | ---: | ---: | ---: |
| 256 bytes | 0.027433 | 0.001379 | 19.89 |
| 4 KiB | 0.042850 | 0.015401 | 2.78 |
| 64 KiB | 0.198501 | 0.176216 | 1.13 |

These measurements compare the Zstd context strategies on synthetic records.
They do not include store lookup, object checksums, tree traversal, or thread-local access.
They do not establish complete Forge performance or allocator CPU percentages.
[Raw timings](decompression-context.json) retain every sample.

All 426 unit tests and all integration and golden suites passed after the decoder change.
The new test mixes sizes and empty content across four threads.
Each thread checks successful decoding after invalid frames and undersized output limits.
Forge and AmberFS consumer validation remains separate.
