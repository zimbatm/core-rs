# Isolated destination membership benchmark

The candidate batch lookup beats scalar lookup in this isolated benchmark.
This does not establish a complete restore speedup.
Forge remains on the previously measured batched-write revision while full-workflow verification remains open.

## Run

Set a new absolute output directory under home:

```console
AMBER_BENCH_DIR=/home/USER/membership-01 nix run .#membership-bench
```

For larger indexes:

```console
AMBER_BENCH_DIR=/home/USER/membership-02 AMBER_BENCH_KEYS=32768 nix run .#membership-bench
```

The default is 8192 keys per segment.
AMBER_BENCH_KEYS accepts values from 1 through 65536.
The directory must not exist.
The runner preserves its synthetic store and result.json there.
It does not open or change existing repositories.
The package contains an optimized Rust test executable and invokes only the ignored membership benchmark.
Normal Core tests skip this benchmark.

## Method

The fixture contains valid encoded blobs with unique eight-byte payloads.
Sequential object IDs make it reproducible.
The benchmark seals exactly 8, 64, then 256 segments.
It measures four query sets at each stage: absent keys, present keys, mixed keys, and adjacent duplicate input.
Each sample processes 32,768 queries in batches of 256.
Each method warms up once before six alternating measured rounds.

The scalar reference uses Store::has and a reusable set for planned input duplicates.
The batch method calls the candidate copy_membership directly.
Both hold the append lock for each input batch.
Both collect result flags into an output vector.
Every measured flag is checked against expected membership after timing.
Duplicates represent an earlier planned write within the same batch.
The benchmark does not perform those writes.

Timing includes append locking, membership lookup, duplicate tracking, and result collection.
It excludes fixture creation, source validation, writes, rotation, synchronization, and result assertions.
It therefore isolates lookup rather than executing either complete copy implementation.
The candidate changes query order but still probes segment filters for absent keys.

Each sample records wall time and /proc/thread-self/schedstat deltas.
The latter cover thread CPU time, run-queue wait, and scheduler timeslices.
The sampling reads slightly extend the CPU interval beyond the wall timer.
Very short samples limit counter precision.
Use wall-time medians as the primary comparison.
Host CPU pressure is captured before and after each sample.

Both runs used CPU 0 and a 4 GiB unit memory limit on bld1.
The first run used 8192 keys per segment.
The second used 32768 keys per segment to increase the index working set.
Each run contains 144 measured samples, or 72 method pairs.
All expected membership checks passed.
Batch lookup won every recorded pair in both runs.

## Results

Values below are median milliseconds per 32,768 queries at 256 sealed segments.

| Keys per segment | Query set | Scalar | Batch | Time reduction |
| --- | --- | ---: | ---: | ---: |
| 8192 | Absent | 73.660 | 46.828 | 36.4% |
| 8192 | Present | 39.072 | 30.196 | 22.7% |
| 8192 | Mixed | 56.384 | 39.021 | 30.8% |
| 8192 | Duplicates | 37.152 | 27.277 | 26.6% |
| 32768 | Absent | 79.214 | 54.303 | 31.4% |
| 32768 | Present | 44.401 | 35.675 | 19.7% |
| 32768 | Mixed | 61.418 | 44.321 | 27.8% |
| 32768 | Duplicates | 39.882 | 29.879 | 25.1% |

Across all segment counts, median reductions range from 17.5 to 36.4 percent in the first run.
They range from 14.1 to 31.4 percent with larger indexes.
The largest recorded per-sample run-queue wait was below 66 microseconds.
The raw samples include every round, method order, and resource observation.

| Complete benchmark | Default indexes | Larger indexes |
| --- | ---: | ---: |
| Program elapsed, seconds | 6.148 | 14.393 |
| Unit CPU, seconds | 6.033 | 14.172 |
| Allocated fixture and report bytes | 211,914,752 | 843,157,504 |
| Peak charged memory, bytes | 225,865,728 | 887,152,640 |

These complete benchmark times include synthetic fixture generation and measured loops.
They exclude package build and process startup.
The small fixtures make repeated diagnosis practical without another 40 GB restore.
Peak charged memory includes the fixture's file cache.

## Evidence and limits

The first build passed 460 Core tests, with the benchmark ignored.
Its benchmark package then passed the default run.
Build unit: forge-dev-7b421d22-1bc5-4110-90d8-9f69645af2d9.service.
Build invocation: ecb72b0e65004ca1a57816746cf541bc.
Source base: f0dcaa71f567b49a13ac4a76162625c77769e822.
Build diff SHA-256: d689b1e2255307447ba744b05c6658ca66370e0d25047cb8a13718b2ec97cbf5.

The final build adds the keys-per-segment parameter.
It also passed 460 Core tests, with one ignored benchmark.
The larger benchmark then passed.
Final build unit: forge-dev-8bc1a1f0-4e52-4422-a567-0afcddb9ed8e.service.
Final build invocation: 0d33e94fba4e444d8f6ea1297d0718cb.
Final build diff SHA-256: 745b28a1f18b9bc07f9c15b58c3015ee4c12c781af7ad49de272d58789962d68.

Default run: forge-membership-isolated-01.service.
Invocation: aa6090e6704541be84a2a9e04434886e.
Artifacts: /home/zimbatm/forge-dev/mb8g0.
[Default evidence](membership-benchmark.json).

Larger run: forge-membership-isolated-02.service.
Invocation: e918d82c96f140d3bc014ebca5e03d5d.
Artifacts: /home/zimbatm/forge-dev/mb8g1.
[Larger-index evidence](membership-benchmark-large.json).

All four units reached MainPID 0, Result success, and ExecMainStatus 0.
The Nix build outputs and full reports remain recorded.

The results show a lookup benefit across the tested synthetic geometries.
They do not explain every cost in the earlier contended full restore.
Validation workers, append traffic, cache competition, and real segment distributions are absent from this measurement.
A fresh full-workflow comparison remains necessary before adopting the membership candidate.
