# fstree builders + read paths — port notes

Scope: `src/fstree/builder.rs` (DirBuilder, IndexBuilder) and
`src/fstree/read.rs` (child_keys, resolve_path/resolve_entry,
collect_entries, lookup_entry, list_entries, write_content, reachable_keys,
check_complete), plus their exports in `src/fstree/mod.rs` and the golden
test `tests/golden_fstree_tree.rs`. Go sources: `fstree/{dir_builder,
index_builder,children,collect,lookup,list,content,reachable,
checkcomplete}.go` at the pinned commit.

## Callback conventions (this unit owns them, per the codec unit's notes)

- **Emit** (Go `Emit func(Object) error`): `&mut F` where
  `F: FnMut(Object) -> Result<(), E>`. Builder methods are generic over one
  `F` per call; internal recursion (`IndexBuilder::add` →
  `close_level` → `add(l+1)`) reuses the same `F`, avoiding the
  `&mut &mut …` infinite-monomorphization trap.
- **Getter** (Go `func(key.Key) ([]byte, error)`):
  `FnMut(Key) -> Result<Vec<u8>, E>` taken by value on the sequential read
  paths (a `&mut closure` also satisfies it, so one getter can serve many
  calls). `reachable_keys`/`check_complete` require
  `Fn(Key) -> Result<Vec<u8>, E> + Sync` with `E: Send` — the Rust spelling
  of Go's "get must be safe for concurrent use".
- `DirBuilder::finish`/`IndexBuilder::finish` consume `self` (Go's Finish
  leaves the builder logically dead; Rust makes reuse a compile error).

## Error mapping

- `BuildError<E> { Encode, Emit, NoChildren }` — Go returns encode and emit
  errors unwrapped, so both variants display transparently. `NoChildren`
  displays `fstree: IndexBuilder.Finish with no children` (verified against
  a Go oracle run).
- `WalkError<E>` — one variant per Go wrap site, same diagnostic text
  (verified against a Go oracle run for every format): `Read`
  (`fstree: reading <key>: …`), `DecodeDirLeaf`/`DecodeDirNode`,
  `ChildKey`, `NotDirObject`, `NotFound`/`NotDir` (with `is_not_found()` /
  `is_not_dir()` helpers standing in for `errors.Is(err, ErrNotFound/
  ErrNotDir)`), `DotDot`, `ContentKey`, `BadLimit`, and — for
  `write_content`, whose Go messages carry **no** `fstree:` prefix —
  `ContentRead` (`reading <key>: …`) and `NotContentObject`. `Children`,
  `Missing`, `Has`, `Codec`, `Io` are returned unwrapped in Go and display
  transparently.
- `MissingObjectError { key }` mirrors Go's `*MissingObjectError`
  (`fstree: object <key> is missing`); `WalkError::missing_object()` is the
  `errors.As` equivalent.
- `ChildKeysError` is its own enum (Go's `ChildKeys` takes no getter, so no
  `E`); `reachable_keys`/`check_complete` wrap it as
  `WalkError::Children` (transparent, as Go returns it unwrapped).
- Names in messages render as `{:?}` of `String::from_utf8_lossy(name)`,
  the convention the codec unit set. Divergence from Go's `%q` (verified
  against a Go oracle run): invalid UTF-8 (Go `"\xff"`, Rust the U+FFFD
  replacement character) **and non-printable characters other than
  `\t`/`\r`/`\n`** (Go `\x01`/`\a`/`\x7f`/`\u200b`, Rust
  `\u{1}`/`\u{7}`/`\u{7f}`/`\u{200b}`). Printable UTF-8 — including
  `\t`, `\r`, `\n`, quotes and backslashes — matches Go byte-for-byte.

## Deliberate API deviations (semantics preserved)

- `list_entries` takes `limit: usize`; Go's negative-limit case is
  unrepresentable, `0` yields the same message (`fstree: ListEntries limit
  must be positive, got 0`). Returns `(Vec<Entry>, bool)` for Go's
  `(entries, more, err)`.
- `check_complete` takes `jobs: usize`; `0` means available parallelism
  (Go: `jobs <= 0` ⇒ `GOMAXPROCS`).
- `resolve_entry` returns `Option<Entry>` for Go's `(*Entry, nil)` — `None`
  for the empty path (the root is not an entry).
- `IndexBuilder::add_child` takes `sep: &[u8]`; file indexes pass `&[]`
  (Go passes `nil`; the two are indistinguishable downstream).
- Go's `ChildKeys` default branch `fstree: unknown object type %s` is
  unreachable in Rust: `key::Type` admits only the five defined types (all
  handled) and `Key::type_()` panics on a non-canonical raw key per that
  method's documented contract.

## Concurrency

- `reachable_keys` and `check_complete` mirror Go's frontier-per-round BFS
  with a jobs-bounded scoped-thread pool (`std::thread::scope` +
  `AtomicUsize` work index). Results are indexed by frontier position, so
  output order is exactly Go's deterministic order; all of a round's tasks
  run even when one fails (as Go's errgroup does). One refinement: when
  several tasks of a round fail, the error returned is the first in
  frontier order (deterministic), while Go's `errgroup.Wait` returns
  whichever failed first in wall time. No caller can rely on the Go
  behavior (it is scheduling-dependent), so this is within contract.
- `reachable_keys` (like Go) never fetches Blob/XattrSet leaves — a Go
  oracle run confirmed a missing blob does **not** error there (only
  `check_complete` reports it, via `has`).
- `parallel_map` runs inline (no threads) when `min(jobs, len) <= 1`.

## Item-encoding details worth noting

- `DirBuilder::add_entry` feeds `IsBoundary` the entry's canonical CBOR
  **map** encoding; Rust reuses `encode::marshal_entries` on a one-entry
  slice and strips the leading `0x81` array head (always exactly one byte),
  avoiding changes to the codec unit's files. Same trick for the DirNode
  index's `[sepName, childKey]` pair item via `marshal_pairs`. The marshal
  happens **before** any builder state changes, as in Go, so a failed entry
  is not retained.
- File indexes feed the raw 32 key bytes; run lengths reset per level on
  `close_level`, and `finish` re-reads `levels.len()` every iteration
  because `add(l+1)` can grow it (Go's loop does the same).
- `finish` at the top level with a single child returns that child's key
  without emitting a wrapper node ("already emitted when created").

## Verification

- **Golden tree** (`tests/golden_fstree_tree.rs`): builds the entire
  VECTORS.md tree through the public APIs (mirroring
  `tools/vectorgen/fstree.go`, which mirrors `ingest/driver.go`, including
  the empty-file ⇒ single empty Blob rule and the ≤ 256-byte xattr inline
  rule); asserts the root key and the deduplicated emitted object set equal
  `fstree/manifest.json` + `objects.bin` **exactly** (every key and every
  byte), then exercises lookup (incl. bigdir through DirNode levels),
  pagination sweeps at several limits against `collect_entries`, content
  reconstruction (big.bin = `data(3, 5242880)` etc.), resolve paths,
  `reachable_keys` == manifest set (root first, no duplicates), and
  `check_complete` (clean; with a deleted Blob ⇒ `MissingObjectError`; with
  a deleted FileNode ⇒ wrapped get error).
- **Differential oracle** (throwaway Go harness against the pinned commit,
  deleted after use): unit tests pin the root key, emitted-object count,
  and a BLAKE3 digest of every emission's `key ‖ bytes` **in emit order**
  for a 5000-entry dir at `ItemChunker(2)` (multi-level DirNode), a
  3-entry dir at bits 7, and a 2000-blob file index at bits 2 — proving
  byte-identical objects *and* identical emit order, not just final roots.
  All error-message strings asserted in tests were captured from the same
  harness.
- All interesting Go tests from the seven `_test.go` files are ported into
  the module `#[cfg(test)]` blocks.
