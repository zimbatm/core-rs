//! The per-entry build logic shared by the sequential walk and the parallel
//! builder (Go: `ingest/driver.go`).

#[cfg(test)]
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
use std::sync::{Arc, mpsc};

use crate::amberignore::{Matcher, descend_opt, ignored_opt};
use crate::cbor;
use crate::chunkers::{self, ItemChunker, SplitError};
use crate::fstree::{self, DirBuilder, IndexBuilder};
use crate::key::Key;

#[cfg(test)]
use super::lock;
use super::{Error, Progress, meta, xattrs};

/// Marker error: the consumer dropped the [`super::ObjectStream`], so the
/// build should unwind quietly (Go: `errStopped`).
#[derive(Debug)]
pub(crate) struct Stopped;

/// The concurrency-safe emit sink shared by all build workers (the Rust
/// shape of Go's `fstree.Emit` closure; `&self` because sibling subtrees
/// emit concurrently).
pub(crate) trait Emit: Sync {
    /// Hands one built object to the consumer; fails only when the consumer
    /// has stopped pulling.
    fn emit(&self, o: fstree::Object) -> Result<(), Stopped>;
}

/// The production sink: a bounded channel to the [`super::ObjectStream`]
/// consumer. A send failure means the receiver was dropped.
pub(crate) struct ChanSink {
    tx: mpsc::SyncSender<fstree::Object>,
}

impl ChanSink {
    pub(crate) fn new(tx: mpsc::SyncSender<fstree::Object>) -> ChanSink {
        ChanSink { tx }
    }
}

impl Emit for ChanSink {
    fn emit(&self, o: fstree::Object) -> Result<(), Stopped> {
        self.tx.send(o).map_err(|_| Stopped)
    }
}

/// A test/oracle sink collecting every emission into a key → bytes map.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MapSink(pub Mutex<HashMap<Key, Vec<u8>>>);

#[cfg(test)]
impl Emit for MapSink {
    fn emit(&self, o: fstree::Object) -> Result<(), Stopped> {
        lock(&self.0).insert(o.key, o.bytes);
        Ok(())
    }
}

/// One directory entry as the walk sees it: raw name bytes plus the readdir
/// type (Go: `os.DirEntry`, where `IsDir` comes from `d_type`).
pub(crate) struct DirEnt {
    pub name: Vec<u8>,
    pub is_dir: bool,
}

impl DirEnt {
    /// The entry's path inside `dir`.
    pub(crate) fn join(&self, dir: &Path) -> PathBuf {
        dir.join(OsStr::from_bytes(&self.name))
    }
}

/// Reads a directory sorted bytewise by name (Go: `os.ReadDir`).
pub(crate) fn read_dir_sorted(path: &Path) -> Result<Vec<DirEnt>, Error> {
    let io_err = |op: &'static str| {
        move |source: io::Error| Error::Io {
            op,
            path: path.to_path_buf(),
            source,
        }
    };
    let rd = fs::read_dir(path).map_err(io_err("open"))?;
    let mut out = Vec::new();
    for de in rd {
        let de = de.map_err(io_err("readdirent"))?;
        let ft = de.file_type().map_err(io_err("lstat"))?;
        out.push(DirEnt {
            name: de.file_name().into_vec(),
            is_dir: ft.is_dir(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The subdirectory-recursion callback handed to [`Driver::build_entry`]:
/// the sequential walk's own recursion, or the parallel builder's (Go: the
/// `buildDir func(string, *amberignore.Matcher, fstree.Emit)` parameter).
pub(crate) type BuildDirFn<'a> = dyn Fn(&Path, Option<&Matcher>) -> Result<Key, Error> + 'a;

/// The build core: chunking parameters plus the progress sink (Go:
/// `driver`).
pub(crate) struct Driver {
    pub ic: ItemChunker,
    pub byte_opts: Option<chunkers::ByteOpts>,
    pub xattr_inline_max: usize,
    /// `None` disables reporting.
    pub progress: Option<Arc<dyn Progress>>,
}

impl Driver {
    /// Forwards to the progress sink, if any (Go: `driver.fileDone`).
    pub(crate) fn file_done(&self) {
        if let Some(p) = &self.progress {
            p.file_done();
        }
    }

    /// Forwards to the progress sink, if any (Go: `driver.addBytes`).
    fn add_bytes(&self, n: usize) {
        if let Some(p) = &self.progress {
            p.add_bytes(n);
        }
    }

    /// Builds the directory at `path` and returns its root key, emitting
    /// every object in its subtree (children before parents). Entries
    /// excluded by `ign` are skipped; excluded directories are pruned without
    /// being read (Go: `driver.buildDir`, the sequential walk).
    ///
    /// Like its Go counterpart this sequential walk has no production
    /// caller — [`super::objects`] always builds directories through the
    /// parallel builder — but it is the reference oracle the parallel build
    /// is checked against, so it is kept compiled exactly as Go keeps it.
    #[allow(dead_code)]
    pub(crate) fn build_dir(
        &self,
        path: &Path,
        ign: Option<&Matcher>,
        sink: &dyn Emit,
    ) -> Result<Key, Error> {
        let ents = read_dir_sorted(path)?;
        let mut db = DirBuilder::new(self.ic);
        for de in ents {
            if ignored_opt(ign, &de.name, de.is_dir) {
                continue;
            }
            let full = de.join(path);
            let e = self.build_entry(&full, &de.name, ign, sink, &|p, i| {
                self.build_dir(p, i, sink)
            })?;
            db.add_entry(&mut |o| sink.emit(o), e)
                .map_err(Error::from_build)?;
        }
        db.finish(&mut |o| sink.emit(o)).map_err(Error::from_build)
    }

    /// Produces the directory entry for one path, recursing into files and
    /// subdirectories (and emitting their objects) and reading inline
    /// metadata for links and special files. `build_dir` is the function used
    /// to build a subdirectory: the sequential walk's own recursion, or the
    /// parallel builder's (Go: `driver.buildEntry`).
    pub(crate) fn build_entry(
        &self,
        full: &Path,
        name: &[u8],
        ign: Option<&Matcher>,
        sink: &dyn Emit,
        build_dir: &BuildDirFn<'_>,
    ) -> Result<fstree::Entry, Error> {
        let md = fs::symlink_metadata(full).map_err(|source| Error::Io {
            op: "lstat",
            path: full.to_path_buf(),
            source,
        })?;
        let m = meta::entry_meta(&md);
        let mut e = fstree::Entry {
            name: name.to_vec(),
            mode: m.mode,
            uid: m.uid,
            gid: m.gid,
            mtime: m.mtime,
            ..Default::default()
        };

        match m.mode & meta::S_IFMT {
            meta::S_IFREG => {
                let ck = self.build_file(full, sink)?;
                e.content_key = ck.as_bytes().to_vec();
                self.file_done();
            }
            meta::S_IFDIR => {
                let sub = descend_opt(ign, full, name)
                    .map_err(|source| Error::ignore_load(full, source))?;
                let ck = build_dir(full, sub.as_ref())?;
                e.content_key = ck.as_bytes().to_vec();
            }
            meta::S_IFLNK => {
                let target = fs::read_link(full).map_err(|source| Error::Io {
                    op: "readlink",
                    path: full.to_path_buf(),
                    source,
                })?;
                e.link_target = target.into_os_string().into_vec();
            }
            meta::S_IFCHR | meta::S_IFBLK => {
                let (major, minor) = meta::device_numbers(&md);
                e.rdev = vec![major, minor];
            }
            meta::S_IFIFO | meta::S_IFSOCK => {
                // no payload key
            }
            other => {
                return Err(Error::Unsupported {
                    path: full.to_path_buf(),
                    mode: other,
                });
            }
        }

        // Extended attributes (skip symlinks).
        if m.mode & meta::S_IFMT != meta::S_IFLNK {
            let xa = xattrs::read_xattrs(full).map_err(|source| Error::Io {
                op: "xattr",
                path: full.to_path_buf(),
                source,
            })?;
            if !xa.is_empty() {
                let enc = cbor::encode_xattrs(&xa);
                if enc.len() <= self.xattr_inline_max {
                    e.xattrs_in = enc;
                } else {
                    let obj = fstree::encode_xattr_set(&xa);
                    e.xattrs_key = obj.key.as_bytes().to_vec();
                    sink.emit(obj).map_err(|Stopped| Error::Stopped)?;
                }
            }
        }
        Ok(e)
    }

    /// Chunks a regular file into Blobs (emitting each), builds its FileNode
    /// index, and returns the file's root key (Go: `driver.buildFile`).
    pub(crate) fn build_file(&self, full: &Path, sink: &dyn Emit) -> Result<Key, Error> {
        let f = fs::File::open(full).map_err(|source| Error::Io {
            op: "open",
            path: full.to_path_buf(),
            source,
        })?;
        let mut ib = IndexBuilder::new_file(self.ic);
        let mut saw = false;
        let res = chunkers::split_bytes(f, self.byte_opts.as_ref(), |chunk: Vec<u8>| {
            self.add_bytes(chunk.len());
            saw = true;
            let obj = fstree::encode_blob(&chunk);
            let k = obj.key;
            sink.emit(obj).map_err(|Stopped| Error::Stopped)?;
            ib.add_child(&mut |o| sink.emit(o), k, &[])
                .map_err(Error::from_build)
        });
        if let Err(e) = res {
            return Err(match e {
                SplitError::Options(oe) => Error::ChunkOpts(oe),
                SplitError::Io(source) => Error::Io {
                    op: "read",
                    path: full.to_path_buf(),
                    source,
                },
                SplitError::Callback(ce) => ce,
            });
        }
        if !saw {
            let obj = fstree::encode_blob(&[]);
            let k = obj.key;
            sink.emit(obj).map_err(|Stopped| Error::Stopped)?;
            ib.add_child(&mut |o| sink.emit(o), k, &[])
                .map_err(Error::from_build)?;
        }
        ib.finish(&mut |o| sink.emit(o)).map_err(Error::from_build)
    }
}
