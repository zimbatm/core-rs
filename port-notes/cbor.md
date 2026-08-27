# Port notes: `cbor` (Go `cborx/`)

Source: `cborx/cborx.go`, `cborx/cborx_test.go` at the pinned commit.

## API mapping

| Go | Rust |
|----|------|
| `appendHead(b, major, n) []byte` (private) | `pub fn append_head(&mut Vec<u8>, u8, u64)` |
| `appendBStr(b, s) []byte` (private) | `pub fn append_bstr(&mut Vec<u8>, &[u8])` |
| `readHead(b)` (private) | `pub fn read_head(&[u8]) -> Result<(u8, u64, &[u8]), Error>` |
| `readBStr(b)` (private) | `pub fn read_bstr(&[u8]) -> Result<(&[u8], &[u8]), Error>` |
| `EncodeXattrs(map[string][]byte) []byte` | `pub fn encode_xattrs(&BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<u8>` |
| `DecodeXattrs([]byte)` | `pub fn decode_xattrs(&[u8]) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, Error>` |

The Go helpers are package-private; here they are `pub` deliberately — the
Rust `fstree`/`reference` modules hand-roll their canonical CBOR (Go used
fxamacker for those) and are expected to reuse these primitives. `MAJOR_*`
constants (0–5) are exported for readability; passing raw numbers like Go
does works identically.

## Deliberate decisions (reviewer checklist)

- **`read_head` is lax about shortest form — preserved.** Go's `readHead`
  doc comment claims "only the shortest-form arguments emitted by appendHead
  are supported", but the code accepts *any* definite-length head form
  (`0x18 0x05` for 5 parses fine); it only rejects additional info 28–31.
  The task brief said "readers must reject non-shortest heads"; the Go code
  does NOT, and per the port-Go-semantics-exactly rule the Rust reader
  accepts them too (unit tests `read_head_accepts_non_shortest`,
  `decode_xattrs_accepts_non_shortest_heads` pin this). If strictness is
  wanted later it must change in Go first or it breaks read-compat.
- **Keys are `Vec<u8>`, not `String`.** Go `map[string][]byte` keys can hold
  arbitrary bytes (xattr names are not guaranteed UTF-8); Rust `String`
  cannot. `BTreeMap<Vec<u8>, Vec<u8>>` preserves exact semantics, including
  duplicate-key-last-wins on decode (map insert overwrite, same as Go).
- **Sort is by encoded key bytes**, literally comparing `append_bstr`
  encodings like Go's `sort.Slice` comparator — this is length-first order,
  NOT plain bytewise key order (`"z"` sorts before `"ab"`); a BTreeMap's
  natural iteration order would be wrong, hence the explicit sort. Pinned by
  `encode_xattrs_sorts_by_encoding_not_raw_key`.
- **Errors**: `Error` enum with `PartialEq` for matching. `UnexpectedEof`
  stands in for `io.ErrUnexpectedEOF`; `XattrKey`/`XattrValue` mirror Go's
  `%w` wrapping via `Box<Error>` + `std::error::Error::source()`. Display
  strings keep Go's diagnostic content with a `cbor:` prefix (module was
  renamed from `cborx`); the `%q` of the key name in the value-error message
  is approximated with `{:?}` of `String::from_utf8_lossy`.
- **`read_bstr` returns a borrowed subslice** where Go returns a copy.
  Callers that keep data copy it themselves (`decode_xattrs` does); no
  observable difference.
- **No preallocation from attacker-controlled map count — deliberate
  divergence, Go has a DoS here.** Go does `make(map[string][]byte, n)` with
  the decoded (attacker-controlled) pair count. Contrary to folklore, the Go
  runtime does *not* ignore large hints that pass its `hint*bucketSize ≤
  maxAlloc` check: verified against the reference implementation,
  `DecodeXattrs([0xBA,0xFF,0xFF,0xFF,0xFF])` (a 5-byte input claiming 2^32-1
  pairs) eagerly allocates a terabyte-scale bucket array and the process is
  OOM-killed before any pair is read. (Counts ≥ 2^63 wrap negative in
  `makemap64`, clamp to 0, and error cleanly; the dangerous band is roughly
  2^20..2^38.) The Rust port uses `BTreeMap` with no capacity hint, so any
  bogus count just fails at the first missing pair with
  `XattrKey{index:0, UnexpectedEof}` — this is required by the porting
  contract ("never panic on untrusted input"; pinned by
  `decode_xattrs_huge_count_errors_promptly`) and should be listed in the
  compatibility-differences report and reported against the Go original.
- **Validation order matches Go exactly**: head first, then major check,
  then per-pair key/value reads (each wrapping errors with index / name
  context), trailing-bytes check last.

## Tests

All three Go tests ported (`TestAppendHead_ShortestForm` →
`append_head_shortest_form`, extended with 32/64-bit boundaries;
`TestEncodeXattrs_CanonicalSorted`; `TestEncodeXattrs_Empty`), plus reader
tests Go lacks (truncation, ai 28–31 rejection, wrong-major, trailing bytes,
error wrapping, duplicate keys, non-shortest acceptance). No golden file of
its own — fstree/reference vectors exercise this codec end-to-end.

Integration test: `tests/cbor.rs` (public-API smoke; no `common` helpers
needed since there are no vectors). Note: `cargo test cbor` name-filters the
integration tests out (their fn names don't contain "cbor"); use
`cargo test --test cbor` for those.

## Reviewer verification (differential fuzz vs Go)

The reviewer ran a 2517-case differential corpus (random garbage, valid
canonical maps, valid maps with non-shortest heads, single-byte-flip /
truncate / append mutations, handcrafted edges) through Go `DecodeXattrs` +
`EncodeXattrs` re-encode and the Rust port side by side. All 2505
Go-survivable cases produced identical outcomes: same accept/reject decision,
same error class with the same wrapped key-index / value-name structure and
diagnostic numbers, and byte-identical canonical re-encodings. The remaining
12 cases (huge claimed map counts) OOM-kill the **Go** process (see the DoS
note above); Rust rejects them cleanly.

## Shared-file issue (not fixed here, per hard rules)

`cargo fmt --check` fails crate-wide on the pre-existing
`tests/common/mod.rs` (line 68: the `panic!` in `load()` needs wrapping).
That file is owned by the scaffold, so it was left untouched; whoever owns
it should run rustfmt on it. `src/cbor.rs` and `tests/cbor.rs` are
fmt-clean.

## Go PR #3 backport (2026-08-27)

`decode_xattrs` now rejects a map head claiming more pairs than the
remaining bytes could hold (`n > len/2`), as `Error::PairCount`, mirroring
Go's fix. The Go bug (presizing a map from the untrusted count) never
existed here — `BTreeMap` has no presize — so the check is parity-only:
same inputs now fail with the same diagnostic instead of a per-pair
`UnexpectedEof`. One pre-existing test case (`[0xa1]`, one claimed pair,
zero bytes) moved onto the new error, as it does in Go.
