# Borrowed FileNode child bytes

Status: measured Core candidate; not yet pinned by Forge.

The FileNode decoder previously copied each child byte string into a separate heap buffer.
It then parsed those buffers into owned Key values.
Definite byte strings now borrow the validated input bytes during that conversion.
The returned Key vector still owns its data.

The existing well-formedness pass runs before decoding.
Chunked strings, tagged values, arrays, and other accepted representations retain their existing decoding behavior.
The public decoder interface and stored CBOR encoding remain unchanged.

## Allocation measurement

The probe selected the first 4096 distinct FileNode keys in physical segment/footer order from the retained nixpkgs store.
Those objects contained 29,893 child keys.
Each measured round decoded every object ten times.
One warmup and three measured rounds ran in each process.
Three process pairs alternated baseline and candidate order.

| Per measured round | Baseline | Candidate |
| --- | ---: | ---: |
| FileNode decodes | 40,960 | 40,960 |
| Allocation and reallocation calls | 421,810 | 122,880 |
| Requested allocation bytes | 30,238,000 | 20,672,240 |

The candidate removed exactly one allocation per decoded child key in this sample.
Allocation calls fell 70.9%; requested bytes fell 31.6%.
Counts were identical across every measured round and process.

Instrumented median times per process were:

| Pair | Baseline seconds | Candidate seconds |
| --- | ---: | ---: |
| 0 | 0.011178 | 0.007032 |
| 1 | 0.010716 | 0.006716 |
| 2 | 0.010244 | 0.006823 |

These short timings include allocation-counter overhead, result comparison, and deallocation.
They do not establish uninstrumented speed or complete GC performance.
The allocation reduction is the primary measured result.
Requested bytes include full new realloc sizes; they are not peak or live memory.
Selection, object reads, setup, and store opening are outside the measured loop.

## Correctness and provenance

All 466 Core tests passed.
The added differential test compares the borrowed and original owned decoders across byte mutations and truncated inputs.
It includes mixed definite, chunked, and tagged strings.
Existing Go-compatible acceptance and error tests also passed.

The probe verifies each source object hash.
Every timed decode compares the complete returned child-key array.
All six processes produced identical ordered sample manifests and decoded-child digests.
The reference database hash remained unchanged.
No collection or publication ran.

The benchmark uses real FileNode objects, but its physical-order sample does not represent every repository object type.
The original store and raw per-process reports remain on bld1 under /home/forge-dev-builds/file-node-allocation-01.
The remote build receipts describe the unchanged Forge launcher checkout.
The evidence separately records actual Core source paths and hashes.

[Exact source, tests, and measurement evidence](borrowed-file-node-results.json).
