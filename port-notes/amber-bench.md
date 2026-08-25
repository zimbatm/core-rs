# amber-bench — port notes

Port of Go `cmd/amber-bench` at HEAD `a2ff135` (`main.go`,
`clone_darwin.go`, `clone_linux.go`, `clone_other.go` →
`examples/amber-bench.rs`, single file with `#[cfg(target_os = …)]`
`clone_file` variants; `main_test.go` → `tests/amber_bench_smoke.rs`).

## Dataset bit-parity with Go (verified)

The gen phase must produce the identical dataset to the Go harness for the
same flags — that is what makes the Rust benchmark numbers comparable to
the Go CLAUDE.md table. To that end `Pcg` in the example ports Go 1.26
`math/rand/v2` verbatim: the 128-bit LCG state advance with the DXSM output
permutation computed on the **post**-advance state (`NewPCG(seed1, seed2)`
seeds `hi`/`lo` directly, no scrambling), `uint64n` (power-of-two masked
single draw, else Lemire with the `lo < n` pre-check), `IntN`, and
`Shuffle` (top-down Fisher–Yates, `j = uint64n(i+1)`). Do **not** replace
it with a crate: numpy-style `pcg64dxsm` (and `rand_pcg`) output from the
pre-advance state with a different multiplier. `write_random` keeps Go's
exact code shape: per-worker reused 1 MiB buffer, xorshift64* state
`(s+1)*0x9E3779B97F4A7C15|1` (per-file seed `i<<32|n`), shifts 12/25/27,
output `x*0x2545F4914F6CDD1D` little-endian, **only full 8-byte words
freshly written per chunk — the final `size%8` tail bytes are stale buffer
content**, exactly like Go (scheduler-dependent for ≤ 1 MiB files; do not
zero them, dedup and the smoke assertions tolerate it).

Verified against Go 1.26.5 on this machine (30 refs, scale 0.1;
`compare_gen.py`, kept in the session scratchpad): 161 files, name and
size sets identical, **161/161 equal on all bytes except the `size%8`
tail**, 155/161 fully identical (the 6 tail-only diffs are files whose
size isn't a multiple of 8 — expected, Go is nondeterministic there too).
The `Manifests` arrays of the two results files were field-for-field
identical, and the gen log totals match ("fresh 153.00 MiB, shared
40.57 MiB, logical 193.57 MiB (overlap 21.0%)").

## results.json compatibility

Serialized field names are byte-identical to Go's default JSON names
(explicit `#[serde(rename)]` everywhere; `GCRuns`, `PackstoreKiB` etc.
don't survive a `rename_all` rule). `Manifests` keeps Go's `,omitempty`
via `skip_serializing_if = "Vec::is_empty"`. The writer uses
`serde_json::ser::PrettyFormatter::with_indent(b" ")` to match Go
`MarshalIndent(res, "", " ")`'s one-space indent. Every Vec field
deserializes Go's `null` (nil slice) as empty via a `null_vec` helper, so
`--phase report` also works on a Go-written results file; the Rust writer
serializes empty vectors as `[]` where Go writes `null` (shape, not name,
difference). `load_results` tolerates a missing file; every phase saves
the whole file.

## Deviations from Go (beyond flag syntax `--x` for `-x`)

1. **Combined CLI output**: Go's `exec.Cmd.CombinedOutput` interleaves
   stdout+stderr in one buffer; the port reproduces that with
   `std::io::pipe()` (stable since 1.87) — both child streams share one
   pipe, so interleaving is the child's own write order, like Go.
2. **`ExitErr` text**: Rust `ExitStatus` displays as `"exit status: 1"`
   where Go says `"exit status 1"`. Only non-emptiness and log/report
   printing consume it.
3. **Close-error surfaces**: `refstore::Store` closes on Arc drop with no
   error return (Go joins `refs.Close()`'s error); `write_random`/
   `copy_file` lose a close error Go's `f.Close()` would report (no fsync
   in either port, matching Go).
4. **Error joining**: Go's `errors.Join` (gen worker errors, `closeAll`)
   maps to joining the error strings with `\n`.
5. **Log cosmetics**: durations print via Rust `Duration` debug after
   Go-style rounding (`fmt_ms`/`fmt_s`), not Go's `Duration.String()`;
   a still-present deleted ref records Go's literal
   `"ref %d still present: <nil>"` text.
6. **`ingest::dir` returns `(WriteStats, Result<Key, _>)`** (stats survive
   errors) instead of Go's `(key, stats, err)` triple — same data.
7. **`free_bytes`/statfs**: `libc::statfs` on `f_bavail * f_bsize`
   (same reasoning as `gc::free_below`: Darwin's `statvfs` truncates);
   errors leave `FreeBytes` 0 as in Go. Segment counting maps Go's two
   globs onto one `read_dir` filtering `.seg` / `.seg.active` suffixes.
8. **`fresh_target` float math** keeps Go's exact expression shape
   (`(100 * MiB) as f64 * scale`, truncated) so both ports cut files at
   identical byte counts.

## Smoke test mechanics (`tests/amber_bench_smoke.rs`)

`CARGO_BIN_EXE_*` does not exist for examples, so the test spawns
`env!("CARGO") build --example amber-store --example amber-bench` in
`CARGO_MANIFEST_DIR` (adding `--release` when its own executable lives
under `target/release`) and locates the binaries via
`current_exe()/../../examples/`. It then execs the bench end-to-end
(`--refs 30 --scale 0.1 --segment 4194304`, restore set) in a tempdir —
Go's TestSmoke calls `run(cfg, "all")` in-process instead — and asserts
the same facts on the parsed results.json plus the report sections.
Runtime ≈ 8 s (matches Go); it writes ~150 MiB to the tempdir, freed on
drop. It needs the wave-3 CLI gc surface (`--segment-size`, `gc run
--grace/--garbage`, `gc status` with `live `/`last cycle` lines, `restore`)
— present and green at the time of writing.

## For the benchmark rerun (MEMORY practice)

Build with `--release` (`target/release/examples/{amber-store,amber-bench}`);
the harness and CLI take the same store layout as Go, so the CLAUDE.md
table procedure carries over verbatim. Datasets and stores land wherever
`--data`/`--store` point — delete them afterwards (user rule); the example
binaries live under `target/` and stay.

## Results, 2026-08-25, Mac: Go vs Rust mark-sweep (both at a2ff135)

Apple-silicon Mac, 14 cores, 48 GiB RAM, APFS SSD; 256 MiB segments, fsync
on, default chunking; full scale (1000 refs). Both arms back to back with
the dataset regenerated in place; Go arm first. The seeded dataset held
across implementations: 66.16 GiB logical, 49.80 GiB fresh, 24.7 %
duplicate, 35,128 files, 200 packs after ingest, 257,090 objects deduped
in both. The few-object drift (773,800 vs 773,806 stored; 503,955 vs
503,961 marked; 63 vs 64 packs reaped by policy) is the generator's
documented stale-tail nondeterminism moving a handful of chunk cut
points — the Go table shows the same ±1-pack variance between its own
runs.

| step | Go (a2ff135) | Rust port |
| --- | --- | --- |
| gen | 12.3 s | 9.8 s |
| ingest 1000 refs | 59.7 s, 1135 MiB/s logical, 855 MiB/s new | 64.9 s, 1044 / 786 MiB/s |
| — of which ingest.Dir | 54.5 s (steady ~53 ms/ref) | 59.3 s (52 → 71 ms/ref tail decay) |
| ref put (completeness walk) | 5.1 ms/ref median, flat | 5.1 ms/ref median, flat |
| delete 700 refs | 3.14 s (4.5 ms/ref) | 3.20 s (4.6 ms/ref) |
| `gc run` (0.5 line) | 9.97 s (mark 324 ms, sweep 9.5 s); 63 reaped, 5.6 GiB copied, 15.8 GiB freed | **7.78 s** (mark 228 ms, sweep 7.3 s); 64 reaped, 5.7 GiB copied, 16.0 GiB freed |
| `gc run --garbage 0` | 31.59 s (mark 277 ms, sweep 31.2 s); 107 reaped, 19.4 GiB copied | **24.96 s** (mark 197 ms, sweep 24.2 s); 105 reaped, 19.0 GiB copied |
| packstore: ingest → policy → forced | 49.90 → 39.77 → 32.54 GiB | 49.90 → 39.63 → 32.54 GiB |
| refs db | 32–136 KiB (Pebble) | 3.5–4 MiB (redb; by-design difference) |
| integrity | 300/300 complete, 10 restores identical | same |

Shape: churn costs (ref put, delete) are identical; the Rust sweep runs
~20–25 % faster (policy 7.8 vs 10.0 s, forced 25.0 vs 31.6 s) and the
mark ~30 % faster; Go ingested ~8 % faster in this pairing, with the
Rust arm's decay from ref ~600 consistent with running second against a
page cache already saturated by 100 GiB of the Go arm's traffic (the Go
repo's own Mac notes flag the same memory-pressure tail). Reclaim and
end state match: policy frees ~10.1–10.3 GiB on disk, forced reaches
32.54 GiB — 85 % of nominal — on both.
