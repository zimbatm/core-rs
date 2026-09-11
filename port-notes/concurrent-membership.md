# Concurrent membership experiment

Concurrent marking reduced median warm traversal time from 28.987 seconds to 7.248 seconds on nixpkgs.
This is a 4.00-fold speedup and a 75.0 percent elapsed-time reduction.
The experiment remains separate from the runtime verify_membership implementation.
Forge and AmberFS retain their existing Core pins.

## Design

ConcurrentMarkSet owns a captured segment snapshot, atomic bitmap words, and atomic active-record flags.
Its fields are private.
Conversion from MarkSet preserves existing marks.
Each successful atomic claim selects one worker to process a previously unmarked object.
Duplicate claims do not repeat child reads.
Active records take priority over sealed records.
Sealed lookups search newest segments first.

Relaxed atomics arbitrate claims only.
Worker joins synchronize completed work and its results.
Consuming ConcurrentMarkSet reconstructs an ordinary MarkSet and counts its marked bits.
Ownership prevents this conversion while scoped workers still borrow the concurrent set.
The type does not verify content or retain objects against collection.

The benchmark processes batches of at most 4,096 keys across eight workers.
Workers perform marking, interior reads, checksum verification, and child decoding.
The baseline marks serially and batches at most 4,096 interiors across eight read workers.
Both use the same captured membership representation.
No persistent index, storage format, certificate schema, or retention rule changes.

## Validation

All 471 library tests passed on bld1.
The new test compares exact physical bitmaps and active-record flags with scalar marking.
It covers existing marks, missing keys, duplicate physical records, and active and sealed snapshots.
One, two, and eight workers run disjoint and overlapping key workloads.
A barrier starts competing workers together.
The disjoint workload checks concurrent updates to different bits within the same word.

The comparison used CPUs 0–7 on bld1.
One warmup pair preceded three alternating measured pairs.
All eight samples selected 27,369,785 objects and read 9,264,325 interiors.
Every sample passed the sorted-key hash-manifest comparison.
This comparison assumes BLAKE3 collision resistance.
Source reference bytes matched before and after the complete run.
The source store was not copied or sealed.
No experiment builds overlapped the measurement.

| Pair | Serial marking | Concurrent marking |
| --- | ---: | ---: |
| 1 | 29.097 s | 7.248 s |
| 2 | 28.825 s | 7.267 s |
| 3 | 28.987 s | 7.224 s |

Median CPU time remained similar: 40.627 seconds versus 40.653 seconds.
Peak process RSS remained about 18.18 GB, including mapped files.
The elapsed-time gain comes from parallel execution, without a comparable reduction in CPU work.
All measured samples recorded zero major faults.

## Scope

Total timing includes snapshot creation, traversal, and concurrent-set conversion in both directions.
Store opening is measured separately.
Sorted-key verification follows the timed phase and RSS sample.
That verification allocates a large key vector.
These results exclude sealing, fs-verity, signing, Git validation, and publication.
They do not establish complete preparation performance or runtime integration correctness.

Run examples/closure-membership-run.nu with --baseline indexed-parallel --candidate indexed-concurrent --warmup.
Supply the built closure-membership-profile binary, object-store directory, root, and a new output directory under home.

Root: 250246838fa0cf5b880be89f9c5eb731c62356ca436de44b12ac15e8b7344f8c.
Membership digest: c1217d0e2507f2227f027b72ca191a24a43d04f9361584f7b7ceb53e2088a051.

The initial benchmark build failed because its missing-object error used an invalid string conversion.
The corrected build and all library tests passed.
Build: forge-dev-970e9a42-9f76-4ecb-ade1-2d4f42a1ddd9.service.
Measurement: forge-concurrent-membership-01.service.
Invocation: 5ba9f462210c4270a8fd86d1fed6b4ee.
Both completed successfully.

[Raw evidence](concurrent-membership.json) records the source hash, binary hash, derivations, preservation checks, and every sample.
