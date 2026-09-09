//! Captured read-only handles for the exact segment inodes loaded by a store.
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use super::{Error, Store, corrupt, unpoison};

/// A durable segment set captured after sealing the active data.
/// Handles remain readable after unlink, collection, wipe, and store closure.
/// This is not a reachability or completeness proof, and does not make files
/// immutable. A caller must establish those properties separately.
/// Later writes are not included. Retained handles do not keep removed records
/// addressable through Store; callers must exclude collection while relying on
/// this snapshot as evidence about the current store.
#[derive(Debug)]
pub struct SegmentSnapshot {
    files: Vec<(u64, File)>,
    pub(super) segments: Vec<std::sync::Arc<super::footer::SealedSegment>>,
}

impl SegmentSnapshot {
    /// Read-only handles, ordered by segment ID.
    pub fn files(&self) -> impl ExactSizeIterator<Item = (u64, &File)> {
        self.files.iter().map(|(id, file)| (*id, file))
    }
}

impl Store {
    /// Seals pending data and captures the exact loaded segment files.
    /// This forces a segment boundary and synchronization, even with sync=false.
    /// It neither verifies payloads nor enables filesystem immutability.
    pub fn seal_snapshot(&self) -> Result<SegmentSnapshot, Error> {
        let mut ap = self.append_lock();
        {
            let sh = unpoison(self.shared.read());
            if sh.closed {
                return Err(Error::Closed);
            }
            if let Some(message) = &sh.failed {
                return Err(Error::Failed(message.clone()));
            }
        }
        if let Err(error) = self.seal_active(&mut ap) {
            self.set_failed(&error);
            return Err(error);
        }
        let sh = unpoison(self.shared.read());
        let mut files = Vec::with_capacity(sh.sealed.len());
        for segment in &sh.sealed {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&segment.path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.dev() != segment.device
                || metadata.ino() != segment.inode
                || metadata.len() != segment.mm.len() as u64
            {
                return Err(corrupt("captured segment inode changed"));
            }
            files.push((segment.id, file));
        }
        Ok(SegmentSnapshot {
            files,
            segments: sh.sealed.clone(),
        })
    }
}
