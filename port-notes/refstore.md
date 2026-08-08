# refstore — port notes

Port of Go `refstore/refstore.go` (name → reference-record KV layer). Public
API: `Store` (`open`/`put`/`get`/`delete`/`all`/`wipe`), `Record {name, data}`,
`Error` with `is_not_found()`.

## Backend: redb instead of Pebble (by design)

Go embeds Pebble; there is no Rust Pebble, so this port uses **redb** per
PORTING.md. The divergence is *on-disk only* — the public semantics are a
faithful port:

- name bytes → record bytes **verbatim** (the store never inspects records;
  validation belongs to the `reference` codec and the daemon, exactly as
  `refstore.go`'s package comment says — `refstore.go` performs no record
  validation at all, and neither does this port),
- blind unconditional overwrite on `put`,
- typed not-found from `get`/`delete` (`Error::NotFound`, Go `ErrNotFound`,
  message verbatim: `"refstore: reference not found"`),
- `all()` in lexicographic (bytewise) name order — redb orders `&[u8]` keys
  with `slice::cmp`, the same order as Pebble's default comparer,
- `wipe()` commits all deletions atomically (Go: one batch commit) and leaves
  the store usable,
- `open(dir, sync)` creates `dir` as needed (Pebble does this itself).

**Interop consequence:** a `refs/` directory written by Go (Pebble MANIFEST/
SST files) is not openable by Rust and vice versa. Rust writes a single
`refs.redb` file inside `dir`. This is the one deliberately non-interoperable
store component; the record *bytes* inside are identical, so a migration is a
trivial read-all/put-all.

## Layout and locking

- `open(dir, sync)` → `create_dir_all(dir)` + `Database::create(dir/refs.redb)`.
- The `refs` table is created eagerly in `open` (one empty write commit) so
  read paths never see a missing table.
- redb holds an OS file lock: a second concurrent `open` of the same
  directory fails (Pebble's `LOCK` file behaves the same). Errors during open
  are wrapped as `Error::Open` — Go: `"refstore: opening pebble: %w"`, here
  `"refstore: opening redb: {0}"`.

## Durability mapping (`sync` flag)

| Go (`pebble.WriteOptions`) | Rust (redb `Durability`, set per write txn) |
|---|---|
| `pebble.Sync` (`sync=true`) | `Durability::Immediate` — fsync before commit returns |
| `pebble.NoSync` (`sync=false`) | `Durability::Eventual` — data written, fsync deferred |

Both sides give the same guarantee shape: with `sync`, an acknowledged write
survives power loss; without, it survives process death (OS page cache) but
not necessarily power loss. redb applies durability per write transaction, so
the flag is stored and set on every `put`/`delete`/`wipe` transaction.

## Concurrency

Go: readers are lock-free; `writeMu` serializes `Put`/`Delete`/`Wipe` so that
`Delete`'s get-then-delete is linearizable (concurrent deletes of one name
report `ErrNotFound` to all but one caller).

Rust: redb is MVCC — `begin_read` never blocks, `begin_write` is single-writer
(internal lock), which *is* the `writeMu`. `delete` is strictly stronger than
Go's check-then-delete: `Table::remove` returns the previous value inside the
write transaction, so existence check + delete are one atomic step; absent →
the transaction is dropped (implicit abort, nothing written) and
`Error::NotFound` returned. Observable semantics identical (ported
`TestConcurrentDeleteReportsOnce` passes).

## API shape differences (Rust-idiomatic, same semantics)

- `Close() error` → RAII: dropping `Store` closes the DB (redb persists its
  allocator state on drop and releases the file lock). No explicit `close`.
- Pebble's per-operation error values pass through unwrapped in Go; here they
  surface as `Error::Backend(redb::Error)` (`#[error(transparent)]`), with
  `From` impls for redb's per-operation error types.
- `Record.Name string` → `Record { name: String, .. }`. Go's
  `string(it.Key())` losslessly carries arbitrary bytes; Rust `String` cannot,
  so `all()` maps a (through-this-API unreachable, since `put` takes `&str`)
  non-UTF-8 key to `Error::NonUtf8Name` instead of corrupting it.
- `wipe` uses `Table::retain(|_, _| false)` in one transaction rather than
  Go's iterate-into-batch (Go's comment about no literal upper bound covering
  the whole keyspace is a Pebble range-tombstone concern with no redb
  counterpart; `retain` over the full range is exact).

## Tests

- `tests/refstore.rs`: full port of `refstore_test.go` — `TestPutGetDelete`,
  `TestAllSortedByName`, `TestSurvivesReopen`, `TestAllEmpty`,
  `TestConcurrentDeleteReportsOnce` (8 threads + barrier gate),
  `TestWipe` — plus the required byte-exact round-trip of canonical
  `reference::Reference` encodings through put/get/all (verbatim bytes, so
  `Reference::decode`'s canonical-bytes check still passes).
- Unit tests in `src/refstore.rs` cover the backend-specific contract:
  sync-flag → durability mapping, directory auto-creation, double-open file
  lock, empty-name round-trip.
- Adversarial review re-ran the differential harness against Go/Pebble
  (pinned e4fcb60) and encoded the results as two more tests in
  `tests/refstore.rs`: `all_orders_names_bytewise` (Pebble order for names
  incl. empty, embedded NUL, DEL, and multi-byte UTF-8 is pure bytewise:
  `"" 41 5a 61 610062 6162 7a 7f c3a9 c3a97a efbfbf` — redb matches) and
  `wipe_empty_and_twice` (Go `Wipe` on an empty store and double-`Wipe` both
  return nil). Also re-confirmed: `Put(name, nil)` round-trips as an empty
  value (not NotFound), `Delete`/`Get` absent both yield exactly
  `"refstore: reference not found"`, and `errors.Is(err, ErrNotFound)` holds
  for both — matching `Error::NotFound` / `is_not_found()`.

No golden vectors exist for this module: the on-disk format is deliberately
backend-specific (VECTORS.md has no refstore section).
