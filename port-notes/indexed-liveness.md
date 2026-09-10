# Indexed liveness measurements

Status: measured Core candidate; Forge adoption remains pending.

The benchmark classified 27,369,786 physical records from the retained nixpkgs store.
It compared key lookup with captured record positions in one process.
Each live fraction used one key-based warmup and three alternating pairs.
Every pair required identical complete per-segment reports.

Key byte 31 selected clustered membership in footer buckets.
Key byte 30 selected mixed membership within those buckets.
These are synthetic marks, not verified repository closures.

## Results

Median classification time in seconds:

| Live fraction | Clustered key | Clustered indexed | Mixed key | Mixed indexed |
| --- | ---: | ---: | ---: | ---: |
| 0% | 8.259 | 0.023 | 8.922 | 0.023 |
| about 10% | 8.279 | 7.976 | 8.444 | 7.975 |
| about 50% | 8.268 | 4.516 | 8.498 | 4.799 |
| about 90% | 9.199 | 1.808 | 14.214 | 1.547 |
| 100% | 8.258 | 0.295 | 13.728 | 0.412 |

All 30 paired comparisons improved.
The mixed 10% case improved by 5.2–5.6% across pairs.
The mixed 50% case improved by 43.4–43.7% across pairs.
Some other cases had substantial timing variation.
The raw evidence retains every sample, including those cases.

Both units terminated successfully.
The reference database hash remained unchanged across both runs.
The source store and all results remain retained.

## Validation and limits

All 465 Core tests passed with the changed runtime and parity assertions.
The subsequent benchmark build added the selection-byte argument.
Runtime and test source hashes match between those builds.
The evidence includes source hashes, build results, unit identities, and raw measurements.

Timers exclude opening, mark preparation, comparison, and serialization.
This measures record classification, not complete compaction or garbage collection.
It does not establish cold performance or total memory usage.
Small cgroup memory charges exclude mapped pages charged elsewhere.
Normal store opening and closing can recover or seal existing active data.

The public liveness_marked method exposes the same captured-position predicate used by compaction.
The original key-callback interface remains available.
The Forge pin remains unchanged pending downstream validation.

[Raw measurements and validation](indexed-liveness-results.json).
