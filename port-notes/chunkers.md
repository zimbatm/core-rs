# chunkers — port notes

Port of Go `chunkers/` (`byte.go`, `item.go`) plus the vendored upstream
`github.com/PlakarKorp/go-cdc-chunkers@v1.0.3` (`chunkers.go` driver +
`chunkers/ultracdc/ultracdc.go`). The upstream ISC copyright notice is kept
verbatim in `src/chunkers.rs` above the ported code, per PORTING.md.

## API shape

- `ByteOpts { min_size, max_size, normal_size, key }` — mirror of upstream
  `ChunkerOpts` (Go re-exports it as `ByteOpts`). Zero field = that field's
  default (2048 / 10240 / 65536), so `ByteOpts::default()` == `nil` opts in Go.
  `key` is carried for API parity; UltraCDC ignores it, exactly as upstream.
- `split_bytes(reader, Option<&ByteOpts>, FnMut(Vec<u8>) -> Result<(), E>)`
  — Go `chunkers.SplitBytes` fused with the upstream `Chunker.Next` driver.
  Every chunk handed to the callback is an owned `Vec<u8>` (Go copies for the
  same reason: the read buffer is reused).
- `SplitError<E> { Options(OptionsError), Io(std::io::Error), Callback(E) }`.
  Go returns the callback error unchanged; the typed enum necessarily wraps it
  (`Callback`), with a `"chunk callback: "` Display prefix. `OptionsError`
  variants carry upstream's exact `ErrNormalSize`/`ErrMinSize`/`ErrMaxSize`
  message text.
- `ItemChunker::new(bits)`, `is_boundary(enc, run_len)` — exact port of
  `item.go` (`min_run`/`max_run` public like Go's exported fields).

## Go quirks preserved

1. **UltraCDC algorithm, verbatim**: maskS=0x2F / maskL=0x2C, LEST=64,
   per-byte hamming distance to 0xAA (compile-time table, spot-checked against
   upstream's precomputed `hammingDistanceTo0xAA` in a unit test), 8-byte
   aligned stepping from `minSize+8` to `n-8` inclusive, mask switched (every
   iteration, like upstream) once `i >= normalSize`, `i+j` cutpoint return,
   low-entropy `i+8` return, default `n`. Size-clamp switch order preserved:
   `n <= minSize → return n`; `n >= maxSize → n = maxSize`;
   `n <= normalSize → normalSize = n`.
2. **Low-entropy path**: on window equality `outBufWin` and `dist` are
   deliberately NOT updated (later windows keep comparing against the original
   window) and `lowEntropyCount` resets to 0 on any inequality — matched
   exactly.
3. **`dist` masking**: Go computes `uint64(dist) & mask` on an `int`. Rust
   keeps `dist: i64` and converts `as u64` before masking, so semantics match
   even in the (unreachable) negative case.
4. **Go out-of-cap-read quirk**: when `minSize < n < minSize+8`, Go slices
   `data[minSize : minSize+8]` past `len(data)` — legal in Go because the
   bufio-backed slice has spare capacity; the stale bytes are never used since
   the main loop cannot run. Rust guards with an early `return n` instead of
   reading out of bounds. Observable behavior identical.
5. **Driver semantics** (`Chunker.Next`): present up to `MaxSize` buffered
   bytes to the algorithm, emit `[0..cutpoint]`, advance, repeat. Buffered
   bytes are **discarded on a reader error** (Go's `Peek` error path returns
   `nil, err`) — tested. Empty input yields zero chunks (Go's first-call
   `([]byte{}, io.EOF)` empty chunk is skipped by SplitBytes's `len > 0`
   check; Rust simply returns). The upstream `cutpoint < MinSize ⇒ io.EOF`
   quirk needs no counterpart: that case only occurs on the final short tail,
   and the Rust loop terminates when the buffer empties.
6. **Item chunker**: `min_run = max(2^bits/4, 2)`, `max_run = 2^bits * 4`,
   `mask = (1<<bits)-1` with `bits = 0 ⇒ mask = 0` (every item at/after
   `min_run` is a boundary). Check order preserved: `run_len >= max_run` first
   (true), then `run_len < min_run` (false), then LE u64 of the first 8
   BLAKE3 digest bytes `& mask == 0`.

## Deviations / decisions

- **Validation is stricter than Go's runtime behavior.** Upstream v1.0.3
  defines `ChunkerImplementation.Validate` (with the 64 B..1 GiB and
  min < normal < max rules) but `NewChunker` never calls it — only the no-op
  `Setup`. So Go's `SplitBytes` never actually rejects bad options. PORTING.md
  explicitly requires "validate exactly as upstream (`64 ≤ … ≤ 1 GiB`, min <
  normal < max)", so `split_bytes` runs the upstream `Validate` rules (same
  checks, same order: normal → min → max, same messages) on the zero-resolved
  options before chunking. Reviewer: confirm this reading of PORTING.md.
- Go's `NewChunker` mutates the caller's opts struct when filling in defaults;
  Rust resolves into a private copy and leaves the caller's `ByteOpts` alone.
- `bits` is `u32` (Go: `int`). Negative bits would be nonsense; huge bits
  would overflow in Go too.
- Golden `item_chunker.json` runs: the trailing (unterminated) run is appended
  only when non-empty; every vector case satisfies `sum(runs) == len(items)`,
  which the test also asserts.

## Test results (2026-08-08, nix dev shell, rustc/cargo 1.95.0)

- `cargo test chunkers` (lib): 17 unit tests pass — ported Go tests
  (reassembly, empty input, retained copies, item-chunker bounds/determinism/
  bits=0) plus driver/validation/low-entropy/LE-bit-extraction tests.
- `cargo test --test golden_chunkers`: **2/2 pass against the generated
  vectors** (`ultracdc.json`: 11 cases incl. empty, 1-byte, min/min+1,
  max-exact, several-MiB splitmix, const 0xAA + const 0x00 LEST paths, concat
  mixes, custom sizes; `item_chunker.json`: bits 0/4/7/10). Also pass without
  `AMBER_GOLDEN_OPTIONAL` since the vectors are present.
- `cargo fmt --check`: **no diff in chunkers files**. It currently fails on
  other modules' files (`src/amberignore.rs`, `tests/common/mod.rs`) which
  this module must not touch.
- `cargo clippy --all-targets -- -D warnings`: **no chunkers findings**. The
  crate-wide run currently fails on two `collapsible_match` errors in
  `src/amberignore.rs` (that module's owner to fix).
