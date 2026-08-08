//! Filters ingest trees through `.amberignore` files with gitignore
//! semantics: patterns compose per directory down the tree and support
//! negation, `**` globs, dir-only (trailing `/`) and anchored (leading `/`)
//! forms; the last matching pattern wins.
//!
//! This is a faithful port of the Go `amberignore` package together with the
//! matching machinery it uses: go-git's `plumbing/format/gitignore` pattern
//! type and Go's `path/filepath.Match` (Unix flavor, `\` escapes enabled,
//! separator `/`), including their exact character-class, escape, and error
//! behavior. Names are byte strings, exactly as in Go, so non-UTF-8 file
//! names match identically.
//!
//! Go's nil `*Matcher` (used for `--no-ignore`) maps to `Option<Matcher>`:
//! `None` ignores nothing and descends to `None` without touching the
//! filesystem. The [`descend_opt`] and [`ignored_opt`] helpers mirror Go's
//! nil-receiver methods for that case.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

/// The per-directory ignore file honored during ingest.
pub const FILE_NAME: &str = ".amberignore";

/// Outcome of matching one pattern: no match, exclusion, or explicit
/// (negated) inclusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchResult {
    NoMatch,
    Exclude,
    Include,
}

/// Answers whether entries of one directory are ignored, carrying the
/// patterns accumulated from the ingest root down to that directory.
/// Matchers are immutable, so sibling subtrees can [`Matcher::descend`] and
/// match concurrently (pattern lists are shared via `Arc`).
#[derive(Clone, Debug)]
pub struct Matcher {
    /// Path of this matcher's directory relative to the root.
    rel: Vec<Vec<u8>>,
    patterns: Arc<Vec<Pattern>>,
}

impl Matcher {
    /// Returns the matcher for the ingest root, loading
    /// `<root_dir>/.amberignore` if present.
    pub fn root(root_dir: impl AsRef<Path>) -> io::Result<Matcher> {
        load(root_dir.as_ref(), Vec::new(), None)
    }

    /// Returns the matcher for the subdirectory `name` of `self`'s directory
    /// (absolute path `abs_dir`), loading its `.amberignore` if present.
    pub fn descend(&self, abs_dir: impl AsRef<Path>, name: &[u8]) -> io::Result<Matcher> {
        let mut rel = self.rel.clone();
        rel.push(name.to_vec());
        load(abs_dir.as_ref(), rel, Some(self))
    }

    /// Reports whether the entry `name` (of type `is_dir`) inside `self`'s
    /// directory is excluded. `.amberignore` files are never themselves
    /// excluded, so a restored tree re-ingests to the same root.
    pub fn ignored(&self, name: &[u8], is_dir: bool) -> bool {
        // Only the ignore *file* is protected: a directory named .amberignore
        // is matched like any other directory, mirroring gitignore semantics.
        if !is_dir && name == FILE_NAME.as_bytes() {
            return false;
        }
        let mut path: Vec<&[u8]> = Vec::with_capacity(self.rel.len() + 1);
        path.extend(self.rel.iter().map(|c| c.as_slice()));
        path.push(name);
        self.matches(&path, is_dir)
    }

    /// go-git's `Matcher.Match`: patterns are consulted from the highest
    /// priority (last) down; the first inclusion or exclusion decides.
    fn matches(&self, path: &[&[u8]], is_dir: bool) -> bool {
        for p in self.patterns.iter().rev() {
            match p.matches(path, is_dir) {
                MatchResult::NoMatch => {}
                MatchResult::Exclude => return true,
                MatchResult::Include => return false,
            }
        }
        false
    }
}

/// [`Matcher::descend`] for an optional matcher: `None` (Go's nil `*Matcher`)
/// descends to `None` without reading anything.
pub fn descend_opt(
    m: Option<&Matcher>,
    abs_dir: impl AsRef<Path>,
    name: &[u8],
) -> io::Result<Option<Matcher>> {
    match m {
        None => Ok(None),
        Some(m) => m.descend(abs_dir, name).map(Some),
    }
}

/// [`Matcher::ignored`] for an optional matcher: `None` ignores nothing.
pub fn ignored_opt(m: Option<&Matcher>, name: &[u8], is_dir: bool) -> bool {
    match m {
        None => false,
        Some(m) => m.ignored(name, is_dir),
    }
}

/// Builds the matcher for the directory `dir` (path `rel` relative to the
/// root), extending `parent`'s patterns with `dir/.amberignore` if it exists.
fn load(dir: &Path, rel: Vec<Vec<u8>>, parent: Option<&Matcher>) -> io::Result<Matcher> {
    let data = match fs::read(dir.join(FILE_NAME)) {
        Ok(data) => data,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // No new patterns: share the parent's pattern list, only rel
            // changes.
            let patterns = match parent {
                Some(parent) => Arc::clone(&parent.patterns),
                None => Arc::new(Vec::new()),
            };
            return Ok(Matcher { rel, patterns });
        }
        Err(e) => return Err(e),
    };
    let mut ps: Vec<Pattern> = match parent {
        Some(parent) => parent.patterns.as_ref().clone(),
        None => Vec::new(),
    };
    for line in data.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.starts_with(b"#") || is_blank(line) {
            continue;
        }
        ps.push(Pattern::parse(line, &rel));
    }
    Ok(Matcher {
        rel,
        patterns: Arc::new(ps),
    })
}

/// `strings.TrimSpace(line) == ""`: true iff every rune in `s` is Unicode
/// white space (invalid UTF-8 decodes to U+FFFD, which is not).
fn is_blank(mut s: &[u8]) -> bool {
    while !s.is_empty() {
        let (r, n) = decode_rune(s);
        match char::from_u32(r as u32) {
            Some(c) if c.is_whitespace() => s = &s[n..],
            _ => return false,
        }
    }
    true
}

/// One gitignore pattern, as parsed by go-git's `gitignore.ParsePattern`.
#[derive(Clone, Debug)]
struct Pattern {
    /// Path (relative to the root) of the directory whose `.amberignore`
    /// defined this pattern; the pattern only applies below it.
    domain: Vec<Vec<u8>>,
    /// The pattern body split on `/`.
    parts: Vec<Vec<u8>>,
    inclusion: bool,
    dir_only: bool,
    is_glob: bool,
}

impl Pattern {
    /// go-git's `ParsePattern`: leading `!` negates; trailing unescaped
    /// spaces are trimmed; a trailing `/` restricts to directories; any
    /// remaining `/` makes the pattern anchored (a glob over components).
    fn parse(p: &[u8], domain: &[Vec<u8>]) -> Pattern {
        let mut p = p;
        let mut inclusion = false;
        if p.starts_with(b"!") {
            inclusion = true;
            p = &p[1..];
        }
        if !p.ends_with(b"\\ ") {
            while p.ends_with(b" ") {
                p = &p[..p.len() - 1];
            }
        }
        let mut dir_only = false;
        if p.ends_with(b"/") {
            dir_only = true;
            p = &p[..p.len() - 1];
        }
        let is_glob = p.contains(&b'/');
        Pattern {
            domain: domain.to_vec(),
            parts: p.split(|&b| b == b'/').map(<[u8]>::to_vec).collect(),
            inclusion,
            dir_only,
            is_glob,
        }
    }

    fn matches(&self, path: &[&[u8]], is_dir: bool) -> MatchResult {
        if path.len() <= self.domain.len() {
            return MatchResult::NoMatch;
        }
        for (e, p) in self.domain.iter().zip(path) {
            if e.as_slice() != *p {
                return MatchResult::NoMatch;
            }
        }

        let path = &path[self.domain.len()..];
        let matched = if self.is_glob {
            self.glob_match(path, is_dir)
        } else {
            self.simple_name_match(path, is_dir)
        };
        if !matched {
            return MatchResult::NoMatch;
        }

        if self.inclusion {
            MatchResult::Include
        } else {
            MatchResult::Exclude
        }
    }

    /// A floating (slash-free) pattern: matches the single name against every
    /// path component.
    fn simple_name_match(&self, path: &[&[u8]], is_dir: bool) -> bool {
        for (i, name) in path.iter().enumerate() {
            match fmatch(&self.parts[0], name) {
                Err(BadPattern) => return false,
                Ok(false) => continue,
                Ok(true) => {
                    if self.dir_only && !is_dir && i == path.len() - 1 {
                        return false;
                    }
                    return true;
                }
            }
        }
        false
    }

    /// An anchored pattern: components are matched in sequence, with `**`
    /// traversing zero or more directories.
    fn glob_match(&self, mut path: &[&[u8]], is_dir: bool) -> bool {
        let mut matched = false;
        let mut can_traverse = false;
        for (i, part) in self.parts.iter().enumerate() {
            if part.is_empty() {
                can_traverse = false;
                continue;
            }
            if part.as_slice() == b"**" {
                if i == self.parts.len() - 1 {
                    break;
                }
                can_traverse = true;
                continue;
            }
            if part.windows(2).any(|w| w == b"**") {
                return false;
            }
            if path.is_empty() {
                return false;
            }
            if can_traverse {
                can_traverse = false;
                while !path.is_empty() {
                    let e = path[0];
                    path = &path[1..];
                    match fmatch(part, e) {
                        Err(BadPattern) => return false,
                        Ok(true) => {
                            matched = true;
                            break;
                        }
                        Ok(false) => {
                            if path.is_empty() {
                                // If nothing is left then fail.
                                matched = false;
                            }
                        }
                    }
                }
            } else {
                match fmatch(part, path[0]) {
                    Ok(true) => {}
                    Ok(false) | Err(BadPattern) => return false,
                }
                matched = true;
                path = &path[1..];
            }
        }
        if matched && self.dir_only && !is_dir && path.is_empty() {
            matched = false;
        }
        matched
    }
}

/// Go's `filepath.ErrBadPattern`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BadPattern;

const SEPARATOR: u8 = b'/';

/// Go's `filepath.Match` (Unix): reports whether `name` matches the shell
/// file name pattern. `*` and `?` never match `/`; `[...]` classes support
/// ranges and `^` negation; `\` escapes the next byte. The pattern must
/// match all of `name`. `Err(BadPattern)` is returned for malformed patterns
/// exactly when Go returns `ErrBadPattern`.
fn fmatch(mut pattern: &[u8], mut name: &[u8]) -> Result<bool, BadPattern> {
    'pattern: while !pattern.is_empty() {
        let (star, chunk, rest) = scan_chunk(pattern);
        pattern = rest;
        if star && chunk.is_empty() {
            // Trailing * matches rest of string unless it has a /.
            return Ok(!name.contains(&SEPARATOR));
        }
        // Look for match at current position.
        if let Some(t) = match_chunk(chunk, name)? {
            // If we're the last chunk, make sure we've exhausted the name;
            // otherwise we'd give a false result even though we could still
            // match using the star.
            if t.is_empty() || !pattern.is_empty() {
                name = t;
                continue 'pattern;
            }
        }
        if star {
            // Look for match skipping i+1 bytes. Cannot skip /.
            let mut i = 0;
            while i < name.len() && name[i] != SEPARATOR {
                if let Some(t) = match_chunk(chunk, &name[i + 1..])? {
                    // If we're the last chunk, make sure we exhausted the
                    // name.
                    if pattern.is_empty() && !t.is_empty() {
                        i += 1;
                        continue;
                    }
                    name = t;
                    continue 'pattern;
                }
                i += 1;
            }
        }
        return Ok(false);
    }
    Ok(name.is_empty())
}

/// Gets the next segment of `pattern`: a non-star chunk possibly preceded by
/// a star. Returns `(star, chunk, rest)`.
fn scan_chunk(mut pattern: &[u8]) -> (bool, &[u8], &[u8]) {
    let mut star = false;
    while !pattern.is_empty() && pattern[0] == b'*' {
        pattern = &pattern[1..];
        star = true;
    }
    let mut inrange = false;
    let mut i = 0;
    while i < pattern.len() {
        match pattern[i] {
            // Error check handled in match_chunk: bad pattern.
            b'\\' if i + 1 < pattern.len() => i += 1,
            b'[' => inrange = true,
            b']' => inrange = false,
            b'*' if !inrange => return (star, &pattern[..i], &pattern[i..]),
            _ => {}
        }
        i += 1;
    }
    (star, pattern, &pattern[pattern.len()..])
}

/// Checks whether `chunk` matches the beginning of `s`; on success returns
/// `Some(remainder)`. The chunk is all single-character operators: literals,
/// char classes, and `?`. After a match fails the loop keeps processing the
/// chunk, checking that the pattern is well-formed without reading `s`.
fn match_chunk<'a>(mut chunk: &[u8], mut s: &'a [u8]) -> Result<Option<&'a [u8]>, BadPattern> {
    let mut failed = false;
    while !chunk.is_empty() {
        failed = failed || s.is_empty();
        match chunk[0] {
            b'[' => {
                // Character class.
                let mut r = 0i32;
                if !failed {
                    let (rr, n) = decode_rune(s);
                    r = rr;
                    s = &s[n..];
                }
                chunk = &chunk[1..];
                // Possibly negated.
                let mut negated = false;
                if !chunk.is_empty() && chunk[0] == b'^' {
                    negated = true;
                    chunk = &chunk[1..];
                }
                // Parse all ranges.
                let mut matched = false;
                let mut nrange = 0;
                loop {
                    if !chunk.is_empty() && chunk[0] == b']' && nrange > 0 {
                        chunk = &chunk[1..];
                        break;
                    }
                    let (lo, rest) = get_esc(chunk)?;
                    chunk = rest;
                    let mut hi = lo;
                    if chunk[0] == b'-' {
                        let (h, rest) = get_esc(&chunk[1..])?;
                        hi = h;
                        chunk = rest;
                    }
                    matched = matched || (lo..=hi).contains(&r);
                    nrange += 1;
                }
                failed = failed || matched == negated;
            }
            b'?' => {
                if !failed {
                    failed = s[0] == SEPARATOR;
                    let (_, n) = decode_rune(s);
                    s = &s[n..];
                }
                chunk = &chunk[1..];
            }
            c => {
                // A literal byte, possibly `\`-escaped.
                let lit = if c == b'\\' {
                    chunk = &chunk[1..];
                    if chunk.is_empty() {
                        return Err(BadPattern);
                    }
                    chunk[0]
                } else {
                    c
                };
                if !failed {
                    failed = lit != s[0];
                    s = &s[1..];
                }
                chunk = &chunk[1..];
            }
        }
    }
    if failed { Ok(None) } else { Ok(Some(s)) }
}

/// Gets a possibly-escaped character from `chunk`, for a character class.
fn get_esc(mut chunk: &[u8]) -> Result<(i32, &[u8]), BadPattern> {
    if chunk.is_empty() || chunk[0] == b'-' || chunk[0] == b']' {
        return Err(BadPattern);
    }
    if chunk[0] == b'\\' {
        chunk = &chunk[1..];
        if chunk.is_empty() {
            return Err(BadPattern);
        }
    }
    let (r, n) = decode_rune(chunk);
    if r == RUNE_ERROR && n == 1 {
        return Err(BadPattern);
    }
    let nchunk = &chunk[n..];
    if nchunk.is_empty() {
        return Err(BadPattern);
    }
    Ok((r, nchunk))
}

/// Go's `utf8.RuneError` (U+FFFD).
const RUNE_ERROR: i32 = 0xFFFD;

/// Go's `utf8.DecodeRune` over bytes: `(RUNE_ERROR, 0)` for empty input,
/// `(RUNE_ERROR, 1)` for invalid encodings (truncated sequences, overlong
/// forms, surrogates, > U+10FFFF), else the rune and its byte length.
fn decode_rune(s: &[u8]) -> (i32, usize) {
    let Some(&b0) = s.first() else {
        return (RUNE_ERROR, 0);
    };
    if b0 < 0x80 {
        return (i32::from(b0), 1);
    }
    let (size, lo, hi) = match b0 {
        0xC2..=0xDF => (2, 0x80, 0xBF),
        0xE0 => (3, 0xA0, 0xBF),
        0xE1..=0xEC | 0xEE..=0xEF => (3, 0x80, 0xBF),
        0xED => (3, 0x80, 0x9F),
        0xF0 => (4, 0x90, 0xBF),
        0xF1..=0xF3 => (4, 0x80, 0xBF),
        0xF4 => (4, 0x80, 0x8F),
        _ => return (RUNE_ERROR, 1),
    };
    if s.len() < size {
        return (RUNE_ERROR, 1);
    }
    let b1 = s[1];
    if !(lo..=hi).contains(&b1) {
        return (RUNE_ERROR, 1);
    }
    if size == 2 {
        return ((i32::from(b0 & 0x1F) << 6) | i32::from(b1 & 0x3F), 2);
    }
    let b2 = s[2];
    if !(0x80..=0xBF).contains(&b2) {
        return (RUNE_ERROR, 1);
    }
    if size == 3 {
        return (
            (i32::from(b0 & 0x0F) << 12) | (i32::from(b1 & 0x3F) << 6) | i32::from(b2 & 0x3F),
            3,
        );
    }
    let b3 = s[3];
    if !(0x80..=0xBF).contains(&b3) {
        return (RUNE_ERROR, 1);
    }
    (
        (i32::from(b0 & 0x07) << 18)
            | (i32::from(b1 & 0x3F) << 12)
            | (i32::from(b2 & 0x3F) << 6)
            | i32::from(b3 & 0x3F),
        4,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let p: PathBuf = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
    }

    // --- Ports of amberignore_test.go ---

    #[test]
    fn nil_matcher_ignores_nothing() {
        let m: Option<&Matcher> = None;
        assert!(!ignored_opt(m, b"anything", false));
        assert!(!ignored_opt(m, b"anything", true));
        let sub = descend_opt(m, "/nonexistent", b"x").unwrap();
        assert!(sub.is_none());
    }

    #[test]
    fn no_ignore_file_ignores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let m = Matcher::root(dir.path()).unwrap();
        assert!(!m.ignored(b"a.txt", false));
        assert!(!m.ignored(b"dir", true));
    }

    #[test]
    fn root_patterns() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            FILE_NAME,
            "# comment\n\n*.log\nbuild/\n/anchored.txt\n",
        );
        let m = Matcher::root(dir.path()).unwrap();
        let cases: &[(&[u8], bool, bool)] = &[
            (b"app.log", false, true),
            // *.log has no trailing slash: matches dirs too.
            (b"app.log", true, true),
            (b"a.txt", false, false),
            // Dir-only pattern matches the directory...
            (b"build", true, true),
            // ...but not a regular file of the same name.
            (b"build", false, false),
            (b"anchored.txt", false, true),
        ];
        for &(name, is_dir, want) in cases {
            assert_eq!(
                m.ignored(name, is_dir),
                want,
                "ignored({:?}, is_dir={})",
                String::from_utf8_lossy(name),
                is_dir
            );
        }
    }

    #[test]
    fn crlf_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "*.log\r\nbuild/\r\n");
        let m = Matcher::root(dir.path()).unwrap();
        assert!(m.ignored(b"app.log", false), "CRLF *.log must still match");
        assert!(m.ignored(b"build", true), "CRLF build/ must still match");
    }

    #[test]
    fn anchored_pattern_only_matches_at_its_domain() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "/top.txt\n");
        let m = Matcher::root(dir.path()).unwrap();
        let sub = m.descend(dir.path().join("sub"), b"sub").unwrap();
        assert!(m.ignored(b"top.txt", false), "must match at the root");
        assert!(
            !sub.ignored(b"top.txt", false),
            "must not match in a subdirectory"
        );
    }

    #[test]
    fn nested_file_adds_patterns() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "sub/.amberignore", "*.tmp\n");
        let m = Matcher::root(dir.path()).unwrap();
        let sub = m.descend(dir.path().join("sub"), b"sub").unwrap();
        assert!(
            !m.ignored(b"x.tmp", false),
            "sub's patterns must not apply at the root"
        );
        assert!(
            sub.ignored(b"x.tmp", false),
            "*.tmp from sub/.amberignore must apply inside sub"
        );
    }

    #[test]
    fn nested_negation_wins() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "*.log\n");
        write_file(dir.path(), "sub/.amberignore", "!keep.log\n");
        let m = Matcher::root(dir.path()).unwrap();
        let sub = m.descend(dir.path().join("sub"), b"sub").unwrap();
        assert!(
            m.ignored(b"keep.log", false),
            "keep.log must be ignored at the root"
        );
        assert!(
            !sub.ignored(b"keep.log", false),
            "nested negation must re-include keep.log in sub"
        );
        assert!(
            sub.ignored(b"other.log", false),
            "inherited *.log must still apply in sub"
        );
    }

    #[test]
    fn domain_scoped_to_defining_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "sub/.amberignore", "*.tmp\n");
        let m = Matcher::root(dir.path()).unwrap();
        let other = m.descend(dir.path().join("other"), b"other").unwrap();
        assert!(
            !other.ignored(b"x.tmp", false),
            "sub's patterns must not leak into a sibling directory"
        );
    }

    #[test]
    fn patterns_apply_to_deeper_descendants() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "secret*\n");
        let m = Matcher::root(dir.path()).unwrap();
        let sub = m.descend(dir.path().join("sub"), b"sub").unwrap();
        let deeper = sub
            .descend(dir.path().join("sub").join("deeper"), b"deeper")
            .unwrap();
        assert!(
            deeper.ignored(b"secret-2", false),
            "floating pattern must apply at any depth"
        );
    }

    #[test]
    fn double_star_glob() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "doc/**/junk\n");
        let m = Matcher::root(dir.path()).unwrap();
        let doc = m.descend(dir.path().join("doc"), b"doc").unwrap();
        let a = doc.descend(dir.path().join("doc").join("a"), b"a").unwrap();
        assert!(
            a.ignored(b"junk", false),
            "doc/**/junk must match doc/a/junk"
        );
        assert!(
            !m.ignored(b"junk", false),
            "doc/**/junk must not match junk at the root"
        );
    }

    #[test]
    fn amberignore_file_never_self_excluded() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "*\n");
        let m = Matcher::root(dir.path()).unwrap();
        assert!(
            !m.ignored(FILE_NAME.as_bytes(), false),
            ".amberignore must always be ingested"
        );
        assert!(
            m.ignored(b"anything-else", false),
            "'*' must ignore other entries"
        );
        let sub = m.descend(dir.path().join("sub"), b"sub").unwrap();
        assert!(
            !sub.ignored(FILE_NAME.as_bytes(), false),
            "nested .amberignore must always be ingested"
        );
    }

    #[test]
    fn unreadable_ignore_file_fails() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid takes no arguments, touches no memory, and cannot
        // fail.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping: running as root bypasses permission checks");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "*.log\n");
        let p = dir.path().join(FILE_NAME);
        fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            Matcher::root(dir.path()).is_err(),
            "expected an error for an unreadable .amberignore"
        );
        // Restore so the tempdir can be cleaned up on all platforms.
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    }

    // --- Ports of go-git gitignore/pattern_test.go ---

    fn pat(p: &str, domain: &[&str]) -> Pattern {
        let domain: Vec<Vec<u8>> = domain.iter().map(|d| d.as_bytes().to_vec()).collect();
        Pattern::parse(p.as_bytes(), &domain)
    }

    fn pmatch(p: &Pattern, path: &[&str], is_dir: bool) -> MatchResult {
        let path: Vec<&[u8]> = path.iter().map(|c| c.as_bytes()).collect();
        p.matches(&path, is_dir)
    }

    #[test]
    fn pattern_table() {
        use MatchResult::{Exclude, Include, NoMatch};
        struct Case {
            pattern: &'static str,
            domain: &'static [&'static str],
            path: &'static [&'static str],
            is_dir: bool,
            want: MatchResult,
        }
        #[rustfmt::skip]
        let cases = [
            Case { pattern: "!vul?ano", domain: &[], path: &["value", "vulkano", "tail"], is_dir: false, want: Include },
            Case { pattern: "value", domain: &["head", "middle", "tail"], path: &["head", "middle"], is_dir: false, want: NoMatch },
            Case { pattern: "value", domain: &["head", "middle", "tail"], path: &["head", "middle", "tail"], is_dir: false, want: NoMatch },
            Case { pattern: "value", domain: &["head", "middle", "tail"], path: &["head", "middle", "_tail_", "value"], is_dir: false, want: NoMatch },
            Case { pattern: "middle/", domain: &["value", "volcano"], path: &["value", "volcano", "middle", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "volcano/", domain: &["value", "volcano"], path: &["value", "volcano", "tail"], is_dir: true, want: NoMatch },
            Case { pattern: "value", domain: &[], path: &["value", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "value", domain: &[], path: &["head", "value", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "value", domain: &[], path: &["head", "value"], is_dir: false, want: Exclude },
            Case { pattern: "value/", domain: &[], path: &["value", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "value/", domain: &[], path: &["head", "value", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "value/", domain: &[], path: &["head", "value"], is_dir: true, want: Exclude },
            Case { pattern: "value/", domain: &[], path: &["head", "value"], is_dir: false, want: NoMatch },
            Case { pattern: "value", domain: &[], path: &["head", "val", "tail"], is_dir: false, want: NoMatch },
            Case { pattern: "val", domain: &[], path: &["head", "value", "tail"], is_dir: false, want: NoMatch },
            Case { pattern: "v*o", domain: &[], path: &["value", "vulkano", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "vul?ano", domain: &[], path: &["value", "vulkano", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "v[ou]l[kc]ano", domain: &[], path: &["value", "volcano"], is_dir: false, want: Exclude },
            Case { pattern: "v[ou]l[", domain: &[], path: &["value", "vol["], is_dir: false, want: NoMatch },
            Case { pattern: "/value/vul?ano", domain: &[], path: &["value", "vulkano", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "middle/tail/", domain: &["value", "volcano"], path: &["value", "volcano", "middle", "tail"], is_dir: true, want: Exclude },
            Case { pattern: "volcano/tail", domain: &["value", "volcano"], path: &["value", "volcano", "tail"], is_dir: false, want: NoMatch },
            Case { pattern: "value/vul?ano", domain: &[], path: &["value", "vulkano", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "value/vulkano", domain: &[], path: &["value", "volcano"], is_dir: false, want: NoMatch },
            Case { pattern: "value/vul?ano", domain: &[], path: &["value"], is_dir: false, want: NoMatch },
            Case { pattern: "/value/volcano", domain: &[], path: &["value", "value", "volcano"], is_dir: false, want: NoMatch },
            Case { pattern: "**/*lue/vol?ano", domain: &[], path: &["value", "volcano", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "**/*lue/vol?ano", domain: &[], path: &["head", "value", "volcano", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "**/*lue/vol?ano", domain: &[], path: &["head", "value", "Volcano", "tail"], is_dir: false, want: NoMatch },
            Case { pattern: "**/*lue/vol?ano/", domain: &[], path: &["head", "value", "volcano", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "**/*lue/vol?ano/", domain: &[], path: &["head", "value", "volcano"], is_dir: true, want: Exclude },
            Case { pattern: "**/*lue/vol?ano/", domain: &[], path: &["head", "value", "Colcano"], is_dir: true, want: NoMatch },
            Case { pattern: "**/*lue/vol?ano/", domain: &[], path: &["head", "value", "volcano"], is_dir: false, want: NoMatch },
            Case { pattern: "/*lue/vol?ano/**", domain: &[], path: &["value", "volcano", "tail", "moretail"], is_dir: false, want: Exclude },
            Case { pattern: "/*lue/vol?ano/**", domain: &[], path: &["value", "volcano"], is_dir: false, want: Exclude },
            Case { pattern: "/*lue/**/vol?ano", domain: &[], path: &["value", "volcano"], is_dir: false, want: Exclude },
            Case { pattern: "/*lue/**/vol?ano", domain: &[], path: &["value", "middle", "volcano"], is_dir: false, want: Exclude },
            Case { pattern: "/*lue/**/vol?ano", domain: &[], path: &["value", "middle1", "middle2", "volcano"], is_dir: false, want: Exclude },
            Case { pattern: "/*lue/**/vol?ano/", domain: &[], path: &["value", "middle1", "middle2", "volcano"], is_dir: true, want: Exclude },
            Case { pattern: "/*lue/**/vol?ano/", domain: &[], path: &["value", "middle1", "middle2", "volcano"], is_dir: false, want: NoMatch },
            Case { pattern: "/*lue/**/vol?ano/", domain: &[], path: &["value", "middle1", "middle2", "volcano", "tail"], is_dir: false, want: Exclude },
            Case { pattern: "/*lue/**foo/vol?ano", domain: &[], path: &["value", "foo", "volcano", "tail"], is_dir: false, want: NoMatch },
            Case { pattern: "**/head/v[ou]l[kc]ano", domain: &[], path: &["value", "head", "volcano"], is_dir: false, want: Exclude },
            Case { pattern: "**/head/v[ou]l[", domain: &[], path: &["value", "head", "vol["], is_dir: false, want: NoMatch },
            Case { pattern: "/value/**/v[ou]l[", domain: &[], path: &["value", "head", "vol["], is_dir: false, want: NoMatch },
            Case { pattern: "**/android/**/GeneratedPluginRegistrant.java", domain: &[], path: &["packages", "flutter_tools", "lib", "src", "android", "gradle.dart"], is_dir: false, want: NoMatch },
        ];
        for c in &cases {
            let p = pat(c.pattern, c.domain);
            assert_eq!(
                pmatch(&p, c.path, c.is_dir),
                c.want,
                "pattern {:?} (domain {:?}) vs {:?} is_dir={}",
                c.pattern,
                c.domain,
                c.path,
                c.is_dir
            );
        }
    }

    // --- Ports of Go path/filepath TestMatch (the matchTests table) ---

    #[test]
    fn fmatch_table() {
        // (pattern, name, matched, bad_pattern)
        #[rustfmt::skip]
        let cases: &[(&[u8], &[u8], bool, bool)] = &[
            (b"abc", b"abc", true, false),
            (b"*", b"abc", true, false),
            (b"*c", b"abc", true, false),
            (b"a*", b"a", true, false),
            (b"a*", b"abc", true, false),
            (b"a*", b"ab/c", false, false),
            (b"a*/b", b"abc/b", true, false),
            (b"a*/b", b"a/c/b", false, false),
            (b"a*b*c*d*e*/f", b"axbxcxdxe/f", true, false),
            (b"a*b*c*d*e*/f", b"axbxcxdxexxx/f", true, false),
            (b"a*b*c*d*e*/f", b"axbxcxdxe/xxx/f", false, false),
            (b"a*b*c*d*e*/f", b"axbxcxdxexxx/fff", false, false),
            (b"a*b?c*x", b"abxbbxdbxebxczzx", true, false),
            (b"a*b?c*x", b"abxbbxdbxebxczzy", false, false),
            (b"ab[c]", b"abc", true, false),
            (b"ab[b-d]", b"abc", true, false),
            (b"ab[e-g]", b"abc", false, false),
            (b"ab[^c]", b"abc", false, false),
            (b"ab[^b-d]", b"abc", false, false),
            (b"ab[^e-g]", b"abc", true, false),
            (b"a\\*b", b"a*b", true, false),
            (b"a\\*b", b"ab", false, false),
            ("a?b".as_bytes(), "a\u{263a}b".as_bytes(), true, false),
            ("a[^a]b".as_bytes(), "a\u{263a}b".as_bytes(), true, false),
            ("a???b".as_bytes(), "a\u{263a}b".as_bytes(), false, false),
            ("a[^a][^a][^a]b".as_bytes(), "a\u{263a}b".as_bytes(), false, false),
            ("[a-\u{03b6}]*".as_bytes(), "\u{03b1}".as_bytes(), true, false),
            ("*[a-\u{03b6}]".as_bytes(), "A".as_bytes(), false, false),
            (b"a?b", b"a/b", false, false),
            (b"a*b", b"a/b", false, false),
            (b"[\\]a]", b"]", true, false),
            (b"[\\-]", b"-", true, false),
            (b"[x\\-]", b"x", true, false),
            (b"[x\\-]", b"-", true, false),
            (b"[x\\-]", b"z", false, false),
            (b"[\\-x]", b"x", true, false),
            (b"[\\-x]", b"-", true, false),
            (b"[\\-x]", b"a", false, false),
            (b"[]a]", b"]", false, true),
            (b"[-]", b"-", false, true),
            (b"[x-]", b"x", false, true),
            (b"[x-]", b"-", false, true),
            (b"[x-]", b"z", false, true),
            (b"[-x]", b"x", false, true),
            (b"[-x]", b"-", false, true),
            (b"[-x]", b"a", false, true),
            (b"\\", b"a", false, true),
            (b"[a-b-c]", b"a", false, true),
            (b"[", b"a", false, true),
            (b"[^", b"a", false, true),
            (b"[^bc", b"a", false, true),
            (b"a[", b"a", false, true),
            (b"a[", b"ab", false, true),
            (b"a[", b"x", false, true),
            (b"a/b[", b"x", false, true),
            (b"*x", b"xxx", true, false),
        ];
        for &(pattern, name, want, want_err) in cases {
            let got = fmatch(pattern, name);
            let want = if want_err { Err(BadPattern) } else { Ok(want) };
            assert_eq!(
                got,
                want,
                "fmatch({:?}, {:?})",
                String::from_utf8_lossy(pattern),
                String::from_utf8_lossy(name)
            );
        }
    }

    // --- Port-specific coverage ---

    #[test]
    fn decode_rune_matches_go() {
        // (input, rune, size) — including invalid encodings, which Go maps to
        // (RuneError, 1).
        let cases: &[(&[u8], i32, usize)] = &[
            (b"", RUNE_ERROR, 0),
            (b"a", 'a' as i32, 1),
            ("é".as_bytes(), 0xE9, 2),
            ("\u{263a}".as_bytes(), 0x263A, 3),
            ("\u{10348}".as_bytes(), 0x10348, 4),
            ("\u{fffd}".as_bytes(), RUNE_ERROR, 3), // literal U+FFFD is valid
            (b"\x80", RUNE_ERROR, 1),               // bare continuation
            (b"\xc3", RUNE_ERROR, 1),               // truncated 2-byte
            (b"\xc0\xaf", RUNE_ERROR, 1),           // overlong
            (b"\xed\xa0\x80", RUNE_ERROR, 1),       // surrogate
            (b"\xf5\x80\x80\x80", RUNE_ERROR, 1),   // > U+10FFFF
            (b"\xe2\x28\xa1", RUNE_ERROR, 1),       // bad continuation
        ];
        for &(input, r, n) in cases {
            assert_eq!(decode_rune(input), (r, n), "decode_rune({input:?})");
        }
    }

    #[test]
    fn trailing_space_handling() {
        // Trailing spaces are trimmed unless the last one is escaped.
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "trim.me   \nkeep\\ \n");
        let m = Matcher::root(dir.path()).unwrap();
        assert!(m.ignored(b"trim.me", false));
        assert!(!m.ignored(b"trim.me   ", false));
        assert!(m.ignored(b"keep ", false), "escaped trailing space kept");
        assert!(!m.ignored(b"keep", false));
    }

    #[test]
    fn blank_lines_including_unicode_space() {
        // A line of only Unicode white space is blank for Go's
        // strings.TrimSpace and must be skipped, not parsed as a pattern.
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "\u{a0}\n \t \n*.log\n");
        let m = Matcher::root(dir.path()).unwrap();
        assert!(m.ignored(b"a.log", false));
        assert!(!m.ignored("\u{a0}".as_bytes(), false));
    }

    #[test]
    fn non_utf8_names_match_like_go() {
        // Invalid UTF-8 in names decodes as U+FFFD per byte, exactly like Go;
        // '*' and '?' still operate bytewise/runewise the same way.
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "*.bin\n");
        let m = Matcher::root(dir.path()).unwrap();
        assert!(m.ignored(b"\xff\xfe.bin", false));
        assert!(!m.ignored(b"\xff\xfe.txt", false));
    }

    #[test]
    fn empty_component_and_bare_doublestar() {
        // go-git quirks pinned against the Go implementation (reviewer-run
        // oracle): an empty pattern component (`a//b`) just resets the `**`
        // traversal flag, so `a//b` behaves like `a/b`; a bare `**` has no
        // slash, so it is a *simple-name* pattern whose fmatch trailing-star
        // shortcut matches every name at every depth; `**/` is its dir-only
        // variant.
        use MatchResult::{Exclude, NoMatch};
        let empty = pat("a//b", &[]);
        assert_eq!(pmatch(&empty, &["a", "b"], false), Exclude);
        assert_eq!(pmatch(&empty, &["a", "x", "b"], false), NoMatch);
        let bare = pat("**", &[]);
        assert_eq!(pmatch(&bare, &["x"], false), Exclude);
        assert_eq!(pmatch(&bare, &["x", "y", "z"], true), Exclude);
        let bare_dir = pat("**/", &[]);
        assert_eq!(pmatch(&bare_dir, &["x"], false), NoMatch);
        assert_eq!(pmatch(&bare_dir, &["x"], true), Exclude);
    }

    #[test]
    fn missing_subdir_descends_to_shared_parent() {
        // Descending into a directory that does not exist on disk (or has no
        // ignore file) shares the parent's pattern list.
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), FILE_NAME, "*.log\n");
        let m = Matcher::root(dir.path()).unwrap();
        let sub = m
            .descend(dir.path().join("nonexistent"), b"nonexistent")
            .unwrap();
        assert!(Arc::ptr_eq(&m.patterns, &sub.patterns));
        assert!(sub.ignored(b"x.log", false));
    }
}
