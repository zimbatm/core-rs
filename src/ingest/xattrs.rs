//! Extended-attribute reading (Go: `ingest/xattr_common.go`,
//! `xattr_darwin.go`, `xattr_linux.go`).
//!
//! Go lists and fetches attributes with `unix.Listxattr`/`unix.Getxattr` on
//! Darwin (which follow symlinks) and `unix.Llistxattr`/`unix.Lgetxattr` on
//! Linux (which do not); the split is mirrored here with the `xattr` crate's
//! `*_deref` variants. Both packages call this only for non-symlink entries,
//! so the difference is unobservable. ENOTSUP from the list call means "no
//! xattrs" (a filesystem without xattr support); any other list/get failure
//! aborts the build, exactly as in Go.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The errno Go's `Getxattr` surfaces when an attribute vanishes between the
/// list and the fetch (`ENODATA` on Linux, `ENOATTR` on Darwin); the `xattr`
/// crate maps that case to `None`, so the error is restored here.
#[cfg(target_os = "linux")]
const ENOATTR: i32 = libc::ENODATA;
#[cfg(not(target_os = "linux"))]
const ENOATTR: i32 = libc::ENOATTR;

/// Lists and reads an entry's extended attributes. Called only for
/// non-symlink entries. Names are raw bytes (xattr names need not be UTF-8);
/// an entry with no attributes yields an empty map (Go: `readXattrs` +
/// `readAllXattrs`, which return a nil map).
pub(crate) fn read_xattrs(path: &Path) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    #[cfg(target_os = "linux")]
    return read_xattrs_with(path, |p| xattr::list(p), |p, n| xattr::get(p, n));
    #[cfg(not(target_os = "linux"))]
    return read_xattrs_with(
        path,
        |p| xattr::list_deref(p),
        |p, n| xattr::get_deref(p, n),
    );
}

/// Lists xattrs with `list` and reads each with `get` (the `xattr` crate's
/// list/get shapes), split out so the error handling is testable (Go:
/// `readXattrsWith`).
fn read_xattrs_with<I, L, G>(path: &Path, list: L, get: G) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>>
where
    I: Iterator<Item = std::ffi::OsString>,
    L: FnOnce(&Path) -> io::Result<I>,
    G: Fn(&Path, &std::ffi::OsStr) -> io::Result<Option<Vec<u8>>>,
{
    // ENOTSUP from a filesystem without xattr support means "no xattrs",
    // as in tar and rsync.
    let names = match list(path) {
        Ok(names) => names,
        Err(e) if is_unsupported(&e) => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
    };
    let mut m = BTreeMap::new();
    for name in names {
        if name.is_empty() {
            continue; // Go's splitXattrNames drops empty entries
        }
        match get(path, &name)? {
            Some(v) => {
                m.insert(name.as_bytes().to_vec(), v);
            }
            None => return Err(io::Error::from_raw_os_error(ENOATTR)),
        }
    }
    Ok(m)
}

/// Go's `ignoreUnsupported`: ENOTSUP / EOPNOTSUPP (distinct values on
/// Darwin, aliases on Linux).
fn is_unsupported(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(code) if code == libc::ENOTSUP || code == libc::EOPNOTSUPP)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    fn no_get(_: &Path, _: &OsStr) -> io::Result<Option<Vec<u8>>> {
        panic!("get called");
    }

    // Ports of Go xattr_common_test.go.
    #[test]
    fn unsupported_filesystem_means_none() {
        for errno in [libc::ENOTSUP, libc::EOPNOTSUPP] {
            let list = |_: &Path| -> io::Result<std::vec::IntoIter<OsString>> {
                Err(io::Error::from_raw_os_error(errno))
            };
            let m = read_xattrs_with(Path::new("/x"), list, no_get)
                .unwrap_or_else(|e| panic!("errno {errno}: got Err({e}), want empty map"));
            assert!(m.is_empty(), "errno {errno}: got {m:?}, want empty");
        }
    }

    #[test]
    fn other_errors_propagate() {
        let list = |_: &Path| -> io::Result<std::vec::IntoIter<OsString>> {
            Err(io::Error::from_raw_os_error(libc::EACCES))
        };
        let err = read_xattrs_with(Path::new("/x"), list, no_get).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EACCES));
    }

    #[test]
    fn reads_values() {
        let list = |_: &Path| -> io::Result<std::vec::IntoIter<OsString>> {
            Ok(vec![OsString::from("user.a"), OsString::from("user.b")].into_iter())
        };
        let get = |_: &Path, name: &OsStr| -> io::Result<Option<Vec<u8>>> {
            let mut v = b"v-".to_vec();
            v.extend_from_slice(name.as_bytes());
            Ok(Some(v))
        };
        let m = read_xattrs_with(Path::new("/x"), list, get).unwrap();
        assert_eq!(m[b"user.a".as_slice()], b"v-user.a");
        assert_eq!(m[b"user.b".as_slice()], b"v-user.b");
    }
}
