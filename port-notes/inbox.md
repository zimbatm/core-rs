# inbox

Port of Go `inbox/` (`inbox.go`, `entry.go`) → `src/inbox.rs`.

## API mapping

| Go | Rust |
|----|------|
| `Open(dir, store, workers, log)` | `Inbox::open(dir, Arc<Store>, workers, Option<LogFn>)` |
| `workers <= 0` ⇒ `GOMAXPROCS(0)` | `workers == 0` ⇒ `available_parallelism()` |
| `Stage(meta, body) (tmpPath, hash, n, err)` | `stage(&Meta, impl Read) -> io::Result<(PathBuf, [u8; 32], u64)>` |
| `Discard(tmpPath)` | `discard(&Path)` |
| `Commit(tmpPath, bodyHash, root) (added, err)` | `commit(&Path, &[u8], Key) -> io::Result<bool>` |
| `WaitFor(root)` | `wait_for(Key)` |
| `Close() error` (always nil) | `close()` (infallible; **also runs on `Drop`** so forgetting it cannot leak the worker threads — Go would leak goroutines instead) |
| `Meta{Ref, Root, ReceivedAt}` | `Meta{ref_, root, received_at}` (`ref` is reserved; follows `key::Key::type_` precedent) |

`Inbox` holds an `Arc<Store>` (Go shares the `*packstore.Store` pointer).
Worker threads are named `inbox-worker`; `thread::Builder::spawn` can fail
(Go's `go` cannot), in which case `open` joins whatever it started and
returns the error.

## slog → LogFn

Go logs through `*slog.Logger` (nil discards); every call site is
`log.Error(msg, "name", name, "error", err)`. Ported as
`pub type LogFn = Box<dyn Fn(&str) + Send + Sync>` receiving one
preformatted line: `{msg} name={name} error={err}` (name lossy-decoded).
`None` discards. The five messages are verbatim from Go:

- `inbox: unreadable entry on recovery, quarantining`
- `inbox: opening entry failed`
- `inbox: removing processed entry failed`
- `inbox: entry failed processing, quarantining`
- `inbox: quarantine rename failed`

## Meta header codec (entry.go)

Encoding is **byte-identical** to Go (fxamacker `CoreDetEncOptions` +
`NilContainerAsEmpty`): 3-entry map, integer keys 0/1/2 ascending, shortest
heads, nil/empty `Root` ⇒ `0x40`. Pinned by `meta_encoding_matches_go`
against a throwaway Go harness (deleted after use) that also cross-checked
the replica against the real unexported `writeMetaHeader` by staging through
`inbox.Open`/`Stage` and diffing the staged file — bytes matched.

Decoding replicates fxamacker's **default DecMode** as observed live
(`meta_decoding_matches_go` pins ~70 probe outcomes): all five definite head
forms (non-shortest accepted), indefinite-length maps/strings, null/undefined
⇒ zero value (top level too), duplicate keys keep the **first** value and the
duplicate is skipped structurally only, unknown uint/negint/text keys skipped
(text keys still UTF-8-checked; bstr/array/map/tag/primitive keys error),
arrays of 0–255 ints (and null ⇒ 0 elements) decode into `Root`, tags
unwrapped generically with tag 0/1 content-type validation, nesting capped at
32 levels counting arrays/maps/tags, trailing bytes rejected, invalid UTF-8
rejected, i64 range enforced.

Known divergences (corrupt/hand-crafted input only; canonical headers written
by either implementation decode identically on both):

- **Error strings** carry the same diagnostic detail but are not verbatim
  (e.g. no Go type paths); `io::Error` wording also differs (`unexpected
  EOF` vs `failed to fill whole buffer`). Classification is unaffected —
  every decode error is only ever logged and quarantined.
- fxamacker checks well-formedness **and extraneous data** before the
  semantic pass; this decoder interleaves them. Input that is invalid in
  both ways can report the other error of the two. Accept/reject sets match.
- Registered tags beyond 0/1 keep no semantics: tag 2/3 (bignum) content is
  decoded raw where fxamacker would build a `big.Int` and then reject it for
  these field types; tag 55799 etc. are likewise generic.
- Tags and floats *inside* a `Root` array element are rejected; fxamacker's
  behavior there is unpinned (never produced by any writer).
- A corrupt 4-byte length prefix up to ~4 GiB allocates before the read
  fails, same as Go's `make([]byte, n)`.

Go's nil-vs-empty `Root` distinction does not exist (`Vec` only); both encode
to `0x40` and `key::Key::parse` rejects both on read, so it is unobservable.

## Behavior ported as-is (including quirks)

- Durability: `stage` fsyncs the tmp file (close-error checking is subsumed
  by `sync_all`; Rust's implicit close cannot report); `commit` renames then
  fsyncs the inbox dir; processing/quarantine/recovery renames and removals
  are **not** followed by a dir fsync — exactly Go's discipline.
- `commit`/`stage` do **not** check `closed`; committing after `close()`
  publishes an entry no worker will drain (recovered by the next `open`).
  Ported unchanged.
- Recovery: tmp entries removed via unlink-then-rmdir (Go `os.Remove`);
  the dir listing is sorted by name (Go `os.ReadDir`) so the queue order
  matches; directories and non-`.pack` names skipped; unreadable entries
  (open/header/`Key::parse` failure) are quarantined with the rename error
  ignored (Go ignores it there too, unlike `quarantine`).
- Worker loop: FIFO queue, one condvar for both queue and barrier (Go's
  single `sync.Cond`), group count decremented even when processing failed,
  entry deleted from the map at zero.
- `process`: open failure only logs (no quarantine, count still released);
  header failure ⇒ quarantine; `write_parallel` (`verify: true`) failure ⇒
  quarantine with the store/reader error; success ⇒ remove, logging removal
  failure. File names travel as `OsString` (Go strings tolerate non-UTF-8).

## Local infrastructure

- `create_temp` hand-rolls `os.CreateTemp(dir, "stage-*")` (the `tempfile`
  crate is a dev-dependency only): `stage-<u32>` names from `RandomState`,
  mode 0600, `create_new`, 10 000 collision retries.
- Mutex poisoning is absorbed (`unpoison`), consistent with `packstore`.

## Test deviation

`commit_idempotent` (Go `TestCommitIdempotent`) parks the workers with
`close()` before committing. Go's version leaves its worker running and
implicitly bets that `Stage`+`Commit` of the duplicate outruns processing of
the first entry (which would remove it and flip the expected `added=false`);
Rust's heavier thread startup loses that race reliably. With the workers
parked the same idempotency path is exercised deterministically (and the
Commit-after-Close quirk above gets coverage).
