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
- **No preallocation from attacker-controlled map count.** Go does
  `make(map, n)` (its runtime ignores absurd hints); `BTreeMap` has no
  capacity hint, so a bogus huge count just loops until `read_bstr` hits
  EOF on the first missing pair — same observable behavior as Go.
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

## Shared-file issue (not fixed here, per hard rules)

`cargo fmt --check` fails crate-wide on the pre-existing
`tests/common/mod.rs` (line 68: the `panic!` in `load()` needs wrapping).
That file is owned by the scaffold, so it was left untouched; whoever owns
it should run rustfmt on it. `src/cbor.rs` and `tests/cbor.rs` are
fmt-clean.
