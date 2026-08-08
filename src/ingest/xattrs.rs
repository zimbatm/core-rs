//! Extended-attribute reading (Go: `ingest/xattr_common.go`,
//! `xattr_darwin.go`, `xattr_linux.go`).
//!
//! Go lists and fetches attributes with `unix.Listxattr`/`unix.Getxattr` on
//! Darwin (which follow symlinks) and `unix.Llistxattr`/`unix.Lgetxattr` on
//! Linux (which do not); the split is mirrored here with the `xattr` crate's
//! `*_deref` variants. Both packages call this only for non-symlink entries,
//! so the difference is unobservable. Any list/get failure aborts the build,
//! exactly as in Go — there is no error tolerance beyond dropping empty
//! names from the list.

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
    let names = xattr::list(path)?;
    #[cfg(not(target_os = "linux"))]
    let names = xattr::list_deref(path)?;

    let mut m = BTreeMap::new();
    for name in names {
        if name.is_empty() {
            continue; // Go's splitXattrNames drops empty entries
        }
        #[cfg(target_os = "linux")]
        let val = xattr::get(path, &name)?;
        #[cfg(not(target_os = "linux"))]
        let val = xattr::get_deref(path, &name)?;
        match val {
            Some(v) => {
                m.insert(name.as_bytes().to_vec(), v);
            }
            None => return Err(io::Error::from_raw_os_error(ENOATTR)),
        }
    }
    Ok(m)
}
