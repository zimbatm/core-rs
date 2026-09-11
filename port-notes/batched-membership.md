# Batched membership experiment

Batching segment lookups did not improve the complete warm traversal consistently.
The instrumented repeat regressed in all three measured pairs.
The existing verified traversal retains scalar marking.
The experimental mark_many method remains on forge-batched-membership-pilot.
No downstream runtime pin changes to this branch.

## Method

MarkSet::mark_many groups lookups by segment while preserving input-order results.
It checks active records first, then sealed segments newest-first.
Temporary memory grows with the input batch, rather than the complete closure.
The prototype uses batches of at most 256 lookup keys.
Both traversal modes retain batches of at most 4,096 interior objects and eight read workers.
No persistent index, stored format, certificate schema, or retention rule changes.

A new library test compares results and exact physical bitmaps with scalar marking.
It covers active records, several sealed segments, duplicate physical records, missing keys, repeated inputs, existing marks, and empty batches.
It repeats the comparison with batch widths of 1, 2, 7, and 256.
Both builds passed all 471 library tests.

Both comparisons ran on bld1 CPUs 0–7.
Each included one warmup pair followed by three alternating measured pairs.
The source store was retained without copying, sealing, or writing object records.
Reference bytes matched before and after each complete comparison.
No builds overlapped measurements.

All 16 samples selected the same 27,369,785 objects and read 9,264,325 interiors.
Every sample passed comparison of the sorted-key hash manifest.
The manifest stores counts and BLAKE3 digests for groups of 1,024 keys.
Its equality check assumes BLAKE3 collision resistance.

Root: 250246838fa0cf5b880be89f9c5eb731c62356ca436de44b12ac15e8b7344f8c.
Membership digest: c1217d0e2507f2227f027b72ca191a24a43d04f9361584f7b7ceb53e2088a051.

## Initial comparison

| Pair | Scalar marking | Batched marking |
| --- | ---: | ---: |
| 1 | 37.649 s | 30.886 s |
| 2 | 35.528 s | 30.808 s |
| 3 | 29.532 s | 30.877 s |

The first two pairs improved, but the last pair regressed.
The large variation in the scalar samples did not justify adoption.
A repeat added separate marking and reading timers, main-thread scheduler counters, and process fault counters.
The lookup implementation remained unchanged between runs.

## Instrumented repeat

| Pair | Scalar total | Batched total | Scalar marking | Batched marking | Scalar reading | Batched reading |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 28.834 s | 30.138 s | 24.941 s | 26.254 s | 3.890 s | 3.880 s |
| 2 | 31.883 s | 31.920 s | 27.610 s | 27.886 s | 4.270 s | 4.032 s |
| 3 | 29.097 s | 31.012 s | 25.222 s | 27.048 s | 3.872 s | 3.961 s |

Median total time increased from 29.097 seconds to 31.012 seconds, about 6.6 percent.
Median CPU time increased from 40.636 seconds to 42.571 seconds.
Peak process RSS remained about 18.187 GB, including mapped files.
All measured samples recorded zero major faults.
Main-thread queue delay ranged from 0.023 to 0.325 seconds.
Those observed delays are much smaller than the marking phase.
They do not establish the cause of the earlier run's larger variation.

The scalar marking phase occupied about 87 percent of elapsed time in its median sample.
This phase includes pending-key handling, presence lookup, duplicate detection, bit updates, and interior-batch construction.
The reading phase includes worker creation, object lookup, reads, checksums, child decoding, joins, and pending-key insertion.
The timers do not isolate every instruction or memory access within either phase.

Grouping work by segment does not remove the repeated filter probes.
It also adds result and pending-index management.
The next experiment should reduce the serial marking cost itself.
Parallel marking into physical bitmaps is one option that could retain the compact membership representation.
Its correctness and performance remain untested.

## Scope and reproduction

Total timing includes creation of the mark set, traversal, and membership construction.
Opening the store is separate.
Sorted-key verification occurs after timing and the RSS sample.
The verification stage allocates a large key vector and is excluded from the reported peak RSS.
Main-thread counters come from /proc/thread-self/schedstat.
Process CPU and fault counters come from getrusage.
These measurements do not isolate heap allocation time.
They exclude sealing, fs-verity, signing, publication, and Git operations.

Run the existing closure-membership-run.nu with --baseline indexed-parallel --candidate indexed-batch --warmup.
Use the built closure-membership-profile binary, an object-store directory, the root, and a new output directory under home.
The script preserves both warmup and measured samples.

Initial build: forge-dev-38654583-d1d0-4e33-9eec-ad7f941ce0bf.service.
Initial measurement: forge-batched-membership-01.service.
Repeat build: forge-dev-932498db-b149-415e-b295-ce2b14c6a73c.service.
Repeat measurement: forge-batched-membership-02.service.
All completed successfully.
[Raw evidence](batched-membership.json) includes invocation IDs, derivations, source hashes, binary hashes, source-preservation checks, and every sample.
