# fstree: check_complete visited-keys delta (pin e4fcb60 → HEAD a2ff135)

Ports the `fstree/checkcomplete.go` change: `CheckComplete` now returns the
visited keys.

## Signature change

```rust
// old: pub fn check_complete<G, H, E>(root, get, has, jobs) -> Result<(), WalkError<E>>
pub fn check_complete<G, H, E>(
    root: Key,
    get: G,
    has: H,
    jobs: usize,
) -> Result<Vec<Key>, WalkError<E>>
where
    G: Fn(Key) -> Result<Vec<u8>, E> + Sync,
    H: Fn(Key) -> Result<bool, E> + Sync,
    E: Send,
```

Returns the visited keys — root first, then discovery order (per BFS level,
in each parent's child order), each key exactly once via the `seen` set. On
error `Err` is returned with no partial list (Go returns a nil visited list).
Append points mirror Go exactly: `visited` starts as `[root]` alongside the
`seen` map, and each child is pushed at the same moment it enters `seen`/`next`
in the sequential post-level merge. The walker itself (parallel_map per level,
leaf has/get split, decode laxness) is untouched.

## Notes

- On success the list is identical in content AND order to `reachable_keys`'
  output for the same tree (both are frontier-position-deterministic BFS
  discovery order), in Rust and in Go. Tests assert order equality across
  `jobs` settings; that determinism was already documented as
  implementation-happenstance for `reachable_keys` and remains so here — Go
  documents only "root first, then discovery order, each once", which IS
  deterministic given each parent's child order.
- Because errors are picked in index order after the whole level completes
  (pre-existing parallel_map behavior, matching Go's errgroup semantics
  closely enough that all error tests pass unchanged), no keys from a failing
  level ever leak: the list is dropped wholesale on `Err`.
- `port-notes/fstree-builders.md` (earlier wave) describes the old
  `Result<(), _>` shape; superseded by this note.

## Callers outside my ownership

Repo-wide grep found NO Rust callers outside `src/fstree/**` and
`tests/golden_fstree_tree.rs` (both mine, both updated). For the next wave:

- Go `gc/collector.go:119` consumes the new return
  (`keys, err := fstree.CheckComplete(...)`) — the Rust gc port gets the
  mark set roots from this `Vec<Key>`.
- Go `cmd/amber-bench/main.go:669` discards it (`_, err :=`).
- `PORTING.md:148` lists `CheckComplete` in the fstree surface (no signature
  text there, nothing to update, and it's off-limits anyway).
