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
  (`Callback`), but Display forwards the inner error verbatim (`"{0}"`, no
  added prefix — changed from `"chunk callback: {0}"` during review to match
  Go's error message content). `OptionsError` variants carry upstream's exact
  `ErrNormalSize`/`ErrMinSize`/`ErrMaxSize` message text.
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
4. **Go out-of-cap-read quirk** (corrected during review): when
   `minSize < n < minSize+8`, Go slices `data[minSize : minSize+8]` past
   `len(data)`, relying on spare capacity behind the peeked bufio slice. On a
   genuine short **tail** that capacity is always there (bufio slides data to
   the buffer start before a short fill), and the stale bytes are never used
   since the main loop cannot run. But for configs with `maxSize < minSize+8`
   (valid per `Validate`, e.g. min=64/normal=65/max=66) the mid-stream window
   sits at the end of the bufio buffer and **Go panics** ("slice bounds out of
   range") from the second chunk on — reproduced against the real Go
   implementation during review. Rust guards with an early `return n`:
   identical cutpoints wherever Go does not crash, and well-defined uniform
   `maxSize` chunks where Go crashes.
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

## Adversarial review (2026-08-08)

Verified line-by-line against `chunkers/{byte,item}.go` and upstream
go-cdc-chunkers v1.0.3, plus a **differential harness** running the real Go
`chunkers.SplitBytes` next to `split_bytes` on 17 crafted cases outside the
golden set (non-8-aligned min/normal, maskS-vs-maskL discrimination via a
dist-3 window, dist==0 first-check cut, const 0x55/0xAA, perturbed
low-entropy data at strides 7/100/513, short tails 71/72/79/80, 2 MB default
config, extreme 64/65/1 MiB config): **chunk sequences identical on every
case where Go does not crash**. The upstream precomputed
`hammingDistanceTo0xAA` table was verified equal to `popcount(b ^ 0xAA)` for
all 256 entries, so the Rust compile-time table is exact.

Findings & fixes applied during review:

1. (doc, medium) Quirk 4 above originally claimed the Go out-of-len read is
   always legal; in fact Go panics mid-stream when `maxSize < minSize+8`.
   Corrected; Rust behavior unchanged (guard was already equivalent-or-better)
   and now pinned by `split_bytes_pathological_max_below_min_plus_8`.
2. (low) `SplitError::Callback` Display dropped its invented
   `"chunk callback: "` prefix — Go returns the callback error unchanged.
3. (tests) Added Go-verified unit tests: mask switch at the normal point
   (dist-3 window: maskS no-cut vs maskL immediate cut, expected lengths from
   Go), dist==0 cut before any update, equal-windows-take-LEST-path (const
   0x55, where dist would also match), pathological `max < min+8` config, and
   the missing upstream `Validate` boundary cases (min > 1 GiB, max < 64,
   max > 1 GiB, zero-normal resolves-then-validates, tightest valid 64/65/66).

Remaining (accepted, no action): `ItemChunker::new(bits)` takes `u32` and
would overflow-panic in debug for `bits >= 62`; Go's `int` shifts are equally
nonsensical there. `bits` is internal configuration, not untrusted input.

## Test results (2026-08-08, nix dev shell, rustc/cargo 1.95.0)

- `cargo test chunkers` (lib): 21 unit tests pass — ported Go tests
  (reassembly, empty input, retained copies, item-chunker bounds/determinism/
  bits=0) plus driver/validation/low-entropy/LE-bit-extraction tests and the
  four review-added tests above.
- `cargo test --test golden_chunkers`: **2/2 pass against the generated
  vectors** (`ultracdc.json`: 11 cases incl. empty, 1-byte, min/min+1,
  max-exact, several-MiB splitmix, const 0xAA + const 0x00 LEST paths, concat
  mixes, custom sizes; `item_chunker.json`: bits 0/4/7/10). Also pass without
  `AMBER_GOLDEN_OPTIONAL` since the vectors are present.
- `cargo fmt --check`: **no diff in chunkers files**. As of the review pass it
  still fails on `tests/common/mod.rs` (shared file, not touchable by this
  module; one `panic!` call needs rustfmt's multi-line form).
- `cargo clippy --all-targets -- -D warnings`: **passes crate-wide** as of the
  review pass (the earlier `src/amberignore.rs` findings were fixed by that
  module's owner).
