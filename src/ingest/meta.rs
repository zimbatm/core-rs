//! POSIX metadata capture from lstat results (Go: `ingest/meta.go` plus the
//! `unix.S_IF*` constants and `unix.Major`/`unix.Minor` it uses).

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;

/// `S_IFMT` and the file-type values, as `u64` for direct comparison with
/// [`Meta::mode`] (libc's types differ per platform; the values match Go's
/// `unix` package on both Linux and macOS).
pub(crate) const S_IFMT: u64 = libc::S_IFMT as u64;
pub(crate) const S_IFREG: u64 = libc::S_IFREG as u64;
pub(crate) const S_IFDIR: u64 = libc::S_IFDIR as u64;
pub(crate) const S_IFLNK: u64 = libc::S_IFLNK as u64;
pub(crate) const S_IFCHR: u64 = libc::S_IFCHR as u64;
pub(crate) const S_IFBLK: u64 = libc::S_IFBLK as u64;
pub(crate) const S_IFIFO: u64 = libc::S_IFIFO as u64;
pub(crate) const S_IFSOCK: u64 = libc::S_IFSOCK as u64;

/// The POSIX metadata pulled from an lstat result (Go: `meta`).
pub(crate) struct Meta {
    /// Raw `st_mode` (type + perms).
    pub mode: u64,
    pub uid: u64,
    pub gid: u64,
    /// ns since the Unix epoch.
    pub mtime: i64,
}

/// Extracts the raw POSIX metadata from an lstat result (Go: `entryMeta`).
/// The mtime multiplication wraps like Go's `Time.UnixNano` arithmetic.
pub(crate) fn entry_meta(md: &Metadata) -> Meta {
    Meta {
        mode: u64::from(md.mode()),
        uid: u64::from(md.uid()),
        gid: u64::from(md.gid()),
        mtime: md
            .mtime()
            .wrapping_mul(1_000_000_000)
            .wrapping_add(md.mtime_nsec()),
    }
}

/// Returns the major/minor device numbers for a device-node lstat result
/// (Go: `deviceNumbers`).
pub(crate) fn device_numbers(md: &Metadata) -> (u64, u64) {
    let rdev = md.rdev();
    (u64::from(dev_major(rdev)), u64::from(dev_minor(rdev)))
}

// The splits below are ports of golang.org/x/sys/unix `Major`/`Minor` — the
// exact functions Go's ingest calls — so the `[major, minor]` stored in an
// entry is bit-identical to Go's on each platform.

/// x/sys/unix `Major` for Linux (glibc `gnu_dev_major`).
#[cfg(target_os = "linux")]
fn dev_major(dev: u64) -> u32 {
    (((dev & 0x0000_0000_000f_ff00) >> 8) | ((dev & 0xffff_f000_0000_0000) >> 32)) as u32
}

/// x/sys/unix `Minor` for Linux (glibc `gnu_dev_minor`).
#[cfg(target_os = "linux")]
fn dev_minor(dev: u64) -> u32 {
    ((dev & 0x0000_0000_0000_00ff) | ((dev & 0x0000_0fff_fff0_0000) >> 12)) as u32
}

/// x/sys/unix `Major` for Darwin.
#[cfg(not(target_os = "linux"))]
fn dev_major(dev: u64) -> u32 {
    ((dev >> 24) & 0xff) as u32
}

/// x/sys/unix `Minor` for Darwin.
#[cfg(not(target_os = "linux"))]
fn dev_minor(dev: u64) -> u32 {
    (dev & 0xff_ffff) as u32
}
