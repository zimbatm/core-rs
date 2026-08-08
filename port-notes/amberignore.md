# amberignore port notes

Source of truth: `amberignore/amberignore.go` at the pinned commit, plus the
matching machinery it delegates to:

- go-git v5.16.2 `plumbing/format/gitignore/{pattern.go,matcher.go}`
  (`ParsePattern`, `pattern.Match`, `matcher.Match`) — ported into
  `src/amberignore.rs` as `Pattern` and `Matcher::matches`.
- Go 1.26 `path/filepath/match.go` (`Match`, `scanChunk`, `matchChunk`,
  `getEsc`), Unix flavor (`\` escapes enabled, separator `/`) — ported as
  `fmatch`/`scan_chunk`/`match_chunk`/`get_esc`.
- Go `unicode/utf8.DecodeRune` — ported as `decode_rune`, because char-class
  and `?` matching decode runes and Go's exact invalid-UTF-8 behavior
  (`(RuneError, 1)` for truncated/overlong/surrogate/out-of-range sequences)
  is observable in match results. Rust's `str` machinery was deliberately not
  used.

No glob crate is involved anywhere.

## API mapping

| Go | Rust |
|----|------|
| `FileName` | `FILE_NAME` |
| `Root(rootDir) (*Matcher, error)` | `Matcher::root(impl AsRef<Path>) -> io::Result<Matcher>` |
| `(*Matcher).Descend(absDir, name)` | `Matcher::descend(&self, abs_dir, name: &[u8])` |
| `(*Matcher).Ignored(name, isDir)` | `Matcher::ignored(&self, name: &[u8], is_dir)` |
| nil `*Matcher` (--no-ignore) | `Option<Matcher>` + free fns `descend_opt` / `ignored_opt` |

Names and pattern lines are byte slices (`&[u8]`), matching Go strings'
byte-oriented behavior; non-UTF-8 file names match identically. Errors are
`io::Error` (Go returns the `os.ReadFile` `*PathError`; there is no
corrupt-data class in this module). Only `fs.ErrNotExist` (`ErrorKind::
NotFound`) is treated as "no ignore file"; anything else (e.g. EACCES,
ENOTDIR) propagates, as in Go.

Matchers are immutable; the pattern list is shared via `Arc`, mirroring Go
sharing the parent's `gitignore.Matcher` when a subdirectory has no
`.amberignore` (only `rel` changes). `missing_subdir_descends_to_shared_parent`
asserts the `Arc::ptr_eq` sharing.

## Go quirks preserved (reviewer checklist)

- **`.amberignore` self-protection is file-only**: `Ignored(FILE_NAME, false)`
  is always false, but a *directory* named `.amberignore` is matched normally
  (golden case `ignore-everything-but-amberignore` covers both).
- **Comments**: only a literal `#` as the *first byte* of the line (after CR
  strip); go-git has no `\#` escape. Leading whitespace makes it a pattern.
- **Blank lines**: skipped iff `strings.TrimSpace(line) == ""`, i.e. every
  *rune* is Unicode whitespace. A line of invalid UTF-8 is not blank.
  (`is_blank` decodes runes exactly like Go.)
- **CR handling**: exactly one trailing `\r` is stripped per line
  (`TrimSuffix`), nothing else.
- **Trailing spaces**: all trailing ASCII spaces trimmed *unless* the line
  ends with `\` + space, in which case nothing is trimmed (go-git's
  `HasSuffix(p, "\\ ")` check — note it protects all earlier trailing spaces
  too).
- **Negation prefix `!` is stripped before the trailing-space check**, then
  trailing `/` (dir-only) after trimming; `is_glob` is computed after the
  trailing `/` strip.
- **Domain scoping**: a pattern only applies strictly *below* its defining
  directory (`path.len() <= domain.len()` ⇒ NoMatch); anchored patterns
  therefore re-anchor at the directory whose file defines them (golden case
  `nested-anchored`).
- **Last match wins**: `matcher.Match` walks patterns from last to first and
  the first Include/Exclude decides.
- **go-git `globMatch` oddities**, ported verbatim:
  - trailing `**` component: loop `break`s, so `logs/**` matches `logs`
    itself (matched flag already set by the `logs` component);
  - a component *containing* `**` mixed with text (e.g. `**foo`) makes the
    whole pattern match nothing (`return false`);
  - empty components (`a//b`, leading `/`) just reset `canTraverse`;
  - in the `**` traversal loop, a non-matching final element sets
    `matched = false` before the loop exits;
  - `filepath.Match` errors (`ErrBadPattern`) make the pattern non-matching
    (both simple and glob paths return false, i.e. NoMatch — never an error
    to the caller).
- **`filepath.Match` fidelity** (`fmatch`): trailing-`*` fast path checks the
  name for `/`; the star-skip loop cannot cross `/`; `?` consumes one *rune*
  and never matches `/`; character classes compare decoded runes with ranges
  and `^` negation; `\` escapes work inside and outside classes;
  *well-formedness is still checked after the match has already failed*
  (`matchChunk` keeps parsing the chunk), so e.g. pattern `a[` fails with
  `ErrBadPattern` for any name. The unit test `fmatch_table` is Go's own
  `matchTests` table (Unix-relevant rows).
- `simpleNameMatch` tries the single pattern component against *every* path
  component, with the dir-only restriction applying only when the match hit
  the final component.

## Golden test harness (`tests/golden_amberignore.rs`)

Each case's `.amberignore` files are materialized into a tempdir and checks
are evaluated the way ingest walks a tree: the matcher descends per parent
component (`is_dir = true` for parents), and an ignored parent directory
prunes the whole subtree — a `!` re-include beneath an excluded directory has
no effect. That pruning rule is pinned by the `no-reinclude-under-ignored-dir`
vector (`!node_modules/keep.js` under `node_modules/` stays ignored) and
mirrors gitignore/ingest behavior. Vectors are present and pass.

## Adversarial review (2026-08-08)

Line-by-line comparison against `amberignore/amberignore.go`, go-git v5.16.2
`gitignore/{pattern.go,matcher.go}` and Go 1.26.3
`path/filepath/match.go` + `unicode/utf8.DecodeRune` found **no semantic
drift**. Note for future readers: Go 1.26's `filepath.Match` has **no**
"validate the remaining pattern after a mismatch" loop (older release notes
suggest otherwise) — `fmatch` correctly returns `Ok(false)` there; the
after-failure syntax checking lives inside `matchChunk`, which is ported.

Differential testing (temporary harnesses, since removed; generators lived in
`/tmp/amberdiff{,2}`; splitmix-seeded, reproducible):

- 100 000 random `filepath.Match` cases (metachar-heavy patterns incl. `\`
  escapes, classes, invalid UTF-8) vs `fmatch`: 0 mismatches, including
  `ErrBadPattern` classification.
- 100 000 random `gitignore.ParsePattern`+`Match` cases (random domains,
  paths, isDir) vs `Pattern::parse`+`matches`: 0 mismatches.
- 3 000 random `.amberignore` *trees* (24 000 checks) walked through
  `Root`/`Descend`/`Ignored` with the ingest prune rule, Go vs Rust
  end-to-end: 0 mismatches (exercises line parsing: comments, blank/unicode
  blank lines, `\r`, `!`, dir-only, inheritance).

The permanent test `empty_component_and_bare_doublestar` was added for two
quirks no ported test pinned: `a//b` (empty component resets `**` traversal ⇒
behaves like `a/b`) and bare `**`/`**/` (no slash ⇒ simple-name pattern via
the trailing-star shortcut, matching every name at every depth). Expected
values were taken from the Go implementation.

Both test tables were checked complete: all 46 go-git `pattern_test.go` cases
and all 56 Go `matchTests` rows are present verbatim.

Minor accepted deviation: Go's `os.ReadFile` error is a `*PathError`
carrying the file path; Rust propagates plain `io::Error` (kind preserved,
path not included in the message). Callers match on `ErrorKind`, as ingest
will; no behavioral difference.

## Known issues outside this module

- ~~`tests/common/mod.rs` fails `cargo fmt --check`~~ — resolved by its owner;
  repo-wide `cargo fmt --check` passes as of this review.
- `cargo clippy --all-targets -- -D warnings` currently fails repo-wide on
  `src/binaryfuse.rs` (10 errors: `excessive_precision`, `manual_div_ceil`)
  — another module's file, its owner must fix. No amberignore file has any
  clippy finding.
- The ingest-level `.amberignore` tests (`ingest/amberignore_test.go`:
  filtered-vs-pruned root equality, scan parity, NoIgnore) belong to the
  `ingest` module port; the matcher-level semantics they rely on are all
  covered here.
