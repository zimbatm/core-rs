# Closure membership traversal experiment

This benchmark compares two ways to construct membership for a complete Amber closure.
It does not change the runtime or establish a publication certificate.

The list method uses check_complete with eight workers.
It verifies interior content keys and checks leaf presence.
It retains the returned key vector, then marks each key in a captured MarkSet.

The indexed method uses serial depth-first traversal.
MarkSet provides duplicate detection and presence checks.
Interior reads verify content keys before decoding children.
It retains a pending stack instead of a complete key vector and separate hash set.

Both methods start with a fresh MarkSet over the same store.
Both fail on missing reachable records or invalid interior content.
Neither hashes blob payloads.
Neither uses previously certified boundaries.

## Measurement

Run closure-membership-profile STORE ROOT NEW_OUTPUT_DIRECTORY MODE.
MODE is list or indexed.
Paths must be absolute. The output directory must be new.
The source must remain stable throughout comparison.

The timer includes MarkSet creation, traversal, and membership construction.
Store opening has a separate timer.
The reported CPU interval covers the timed phase.
Peak RSS covers process startup, store opening, and the timed phase.
RSS includes file mappings and does not measure isolated heap memory.

Verification follows timing and RSS sampling.
It enumerates selected keys through the store, sorts them, and removes physical duplicates.
The resulting key count must equal MarkSet::marked.
It records BLAKE3 over the complete sorted key sequence.
A compact manifest contains a length and BLAKE3 digest for each group of 1,024 keys.
The paired runner requires identical counts, read counts, root identity, digest, and manifest bytes.
Membership comparison assumes BLAKE3 collision resistance.

The verification pass can allocate a large key vector.
Its allocation and CPU costs are excluded from the measured candidate traversal.
This exclusion is necessary to measure the representation being compared.

The versioned Nushell runner executes three alternating process pairs.
The first pair starts with list mode. There is no separate warmup.
The host's filesystem caches are not cleared.
The unit uses bld1 CPUs 0–7 and a 112 GiB memory limit.

This experiment excludes sealing, fs-verity, checkpoint encoding, synchronization, signing, Git validation, and publication.
Indexed membership time is integrated into traversal.
Its separate membership_seconds field is zero because there is no separate phase.

## Initial failed run

The first benchmark attempted to export sealed membership after traversal.
The retained source has active records, so Core correctly rejected that conversion.
Unit forge-closure-membership-01.service terminated with exit code 1.
Invocation: b73ecd6f5a0b4e26b28c466087d7eb7a.
The failed result directory remains under /home/forge-dev-builds/closure-membership-01.
No performance result from that run is accepted.

The corrected verifier supports active records without sealing the source.
This changes benchmark verification only.
It does not weaken the sealed-membership API or certificate rules.

The second run completed its first Rust sample, then failed in the Nushell runner.
An untyped null variable could not accept the identity record.
Unit forge-closure-membership-02.service terminated with exit code 1.
Invocation: 02a716eaba7f4c878f126f3e30520972.
The runner now declares that variable as any.
The third run uses the same compiled Rust benchmark with the corrected runner.

## Bounded parallel candidate

The indexed-parallel method marks keys serially in the captured MarkSet.
It batches at most 4,096 unvisited interior keys.
Up to eight scoped workers read, hash-check, and decode that batch.
Each worker returns child keys. The main thread appends them to its pending stack.

The batch limit bounds simultaneous interior reads.
It does not impose a hard byte limit on the pending stack or decoded child lists.
The reported peak_pending counts only the main stack.
Peak RSS includes worker allocations during the timed phase.

Select this candidate with --candidate indexed-parallel in the Nushell runner.
The list baseline remains the same eight-worker check_complete implementation.

## Results on retained nixpkgs

Each sample selected 27,369,785 objects and read 9,264,325 interiors.
All twelve completed samples produced the same sorted-key digest and hash manifest.
The retained reference bytes stayed unchanged.

The serial candidate reduced peak RSS by about 4.62 GB.
Its median elapsed time was 46.778 seconds, versus 40.004 seconds for its paired list baseline.
Timing varied substantially across those serial pairs.
Serial traversal did not provide a consistent latency improvement.

The bounded parallel comparison was more stable:

| Pair | List elapsed | Parallel elapsed | List CPU | Parallel CPU |
| --- | ---: | ---: | ---: | ---: |
| 0 | 31.532645 s | 28.987344 s | 68.837105 s | 40.366484 s |
| 1 | 31.535890 s | 29.014254 s | 68.836427 s | 40.776505 s |
| 2 | 31.522992 s | 28.751186 s | 68.854381 s | 40.282219 s |

Parallel elapsed time improved by 8.0–8.8 percent in every pair.
CPU time decreased by 40.8–41.5 percent.
Peak RSS decreased from about 22.761 GB to 18.197 GB.
The reduction was 4.563–4.565 GB. These are decimal gigabytes.
This is process RSS through the timed phase, including mappings.

The list baseline spent 18.918–19.089 seconds in its separate membership loop.
That was about 60 percent of its measured total.
Its returned key vector alone had 1,073,741,824 bytes of capacity.
The indexed methods eliminated that full vector and the completeness walk's global hash set.
The parallel candidate's pending stack peaked at 903,890 keys.
Both indexed methods encountered 3,085,839 duplicate pops.

This evidence supports integrating direct membership construction for consumers that need membership.
It does not justify replacing a completeness-only check with the more expensive membership operation.
The current runtime remains unchanged.
Complete certificate creation, publication latency, collection behavior, and cold-storage performance remain unmeasured for this candidate.

## Validation and reproduction

The Rust benchmark compiled on bld1. Both paired runs completed successfully.
The runner passed Nushell parsing and executed all comparisons.
No runtime library code changed, so the full Core and Forge suites were not rerun.

Serial unit: forge-closure-membership-03.service.
Serial invocation: 1c8688ff1fab416f90665f6bd5793ea8.
Parallel unit: forge-closure-membership-04.service.
Parallel invocation: a0fced5a254949499f0ecd186febd006.
Both units ended with MainPID 0, Result success, and ExecMainStatus 0.
Raw artifacts remain under /home/forge-dev-builds/closure-membership-03 and closure-membership-04.

Build the closure-membership-bench package on bld1.
Run examples/closure-membership-run.nu with the binary, store, root, and a new output directory.
Add --candidate indexed-parallel for the parallel comparison.
[The result record](closure-membership-results.json) preserves source hashes, raw samples, and build receipts.
