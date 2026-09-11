use crate::{
    key::{Key, Type},
    packstore::{self, MarkSet, SealedMembership, Store},
};

use super::{ChildKeysError, child_keys};

#[derive(Debug, thiserror::Error)]
pub enum MembershipError {
    #[error("membership snapshot: {0}")]
    Snapshot(#[source] packstore::Error),
    #[error("missing reachable object {key}")]
    Missing { key: Key },
    #[error("reading reachable object {key}: {source}")]
    Read {
        key: Key,
        #[source]
        source: packstore::Error,
    },
    #[error("interior checksum mismatch for {key}")]
    InvalidInterior { key: Key },
    #[error("decoding reachable object {key}: {source}")]
    Children {
        key: Key,
        #[source]
        source: ChildKeysError,
    },
    #[error("membership read worker panicked")]
    WorkerPanic,
}

/// Complete closure evidence over captured records. This owns no retention pin.
/// Interior hashes are verified; blob and xattr payload hashes are not.
pub struct VerifiedClosure {
    root: Key,
    marks: MarkSet,
}

impl VerifiedClosure {
    pub fn root(&self) -> Key {
        self.root
    }

    pub fn object_count(&self) -> usize {
        self.marks.marked()
    }

    /// Requires a snapshot with no active records. The returned membership
    /// does not carry the root binding or synchronize and authenticate files.
    pub fn into_sealed_membership(self) -> Result<SealedMembership, packstore::Error> {
        self.marks.into_sealed_membership()
    }
}

/// Verifies a complete closure while constructing membership directly.
/// The caller must exclude collection and record replacement during this walk
/// and subsequent certificate construction. Captured mappings are not pins.
/// No previously verified boundary can skip this walk.
/// Zero jobs uses available parallelism. Each batch has at most 4096 interiors.
pub fn verify_membership(
    store: &Store,
    root: Key,
    jobs: usize,
) -> Result<VerifiedClosure, MembershipError> {
    let jobs = if jobs == 0 {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
    } else {
        jobs
    };
    let mut marks = store.new_mark_set().map_err(MembershipError::Snapshot)?;
    let mut pending = vec![root];
    let mut batch = Vec::with_capacity(4096);
    while !pending.is_empty() {
        batch.clear();
        while batch.len() < 4096 {
            let Some(key) = pending.pop() else { break };
            let (newly, present) = marks.mark(key);
            if !present {
                return Err(MembershipError::Missing { key });
            }
            if newly && !matches!(key.type_(), Type::Blob | Type::XattrSet) {
                batch.push(key);
            }
        }
        if batch.is_empty() {
            continue;
        }
        std::thread::scope(|scope| -> Result<(), MembershipError> {
            let workers: Vec<_> = batch
                .chunks(batch.len().div_ceil(jobs))
                .map(|keys| {
                    scope.spawn(move || -> Result<Vec<Key>, MembershipError> {
                        let mut children = Vec::new();
                        for &key in keys {
                            let data = store
                                .get(key)
                                .map_err(|source| MembershipError::Read { key, source })?;
                            if Key::new(key.type_(), key.length(), &data) != key {
                                return Err(MembershipError::InvalidInterior { key });
                            }
                            children.extend(
                                child_keys(key, &data)
                                    .map_err(|source| MembershipError::Children { key, source })?,
                            );
                        }
                        Ok(children)
                    })
                })
                .collect();
            for worker in workers {
                pending.extend(worker.join().map_err(|_| MembershipError::WorkerPanic)??);
            }
            Ok(())
        })?;
    }
    Ok(VerifiedClosure { root, marks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fstree::{check_complete, encode_blob, encode_file_node};

    fn store() -> (tempfile::TempDir, Store) {
        let directory = tempfile::TempDir::new().unwrap();
        let store =
            Store::open_with(directory.path(), packstore::Options::new().sync(false)).unwrap();
        (directory, store)
    }

    #[test]
    fn membership_matches_complete_walk_across_batches_and_workers() {
        let (_directory, store) = store();
        let mut children = Vec::new();
        for index in 0u64..8200 {
            let blob = encode_blob(&index.to_le_bytes());
            let file = encode_file_node(&[blob.key, blob.key]);
            store.put(blob.key, &blob.bytes).unwrap();
            store.put(file.key, &file.bytes).unwrap();
            children.extend([file.key, file.key]);
        }
        let root = encode_file_node(&children);
        let unrelated = encode_blob(b"outside the closure");
        store.put(root.key, &root.bytes).unwrap();
        store.put(unrelated.key, &unrelated.bytes).unwrap();
        let snapshot = store.seal_snapshot().unwrap();
        let keys = check_complete(root.key, |key| store.get(key), |key| store.has(key), 8).unwrap();
        let mut expected = store.new_mark_set().unwrap();
        for &key in &keys {
            assert!(expected.mark(key).1);
        }
        let expected = expected.into_sealed_membership().unwrap().bitmaps();
        for jobs in [1, 8] {
            let proof = verify_membership(&store, root.key, jobs).unwrap();
            assert_eq!(proof.root(), root.key);
            assert_eq!(proof.object_count(), 16401);
            assert_eq!(proof.object_count(), keys.len());
            let actual = proof.into_sealed_membership().unwrap().bitmaps();
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(actual.segment_id, expected.segment_id);
                assert_eq!(actual.record_count, expected.record_count);
                assert_eq!(actual.words, expected.words);
            }
            let restored = snapshot.restore_membership(&actual).unwrap();
            assert!(keys.iter().all(|key| restored.contains(*key)));
            assert!(!restored.contains(unrelated.key));
        }
    }

    #[test]
    fn missing_records_do_not_produce_evidence() {
        let (_directory, store) = store();
        let absent = encode_blob(b"absent");
        assert!(matches!(verify_membership(&store, absent.key, 0),
            Err(MembershipError::Missing { key }) if key == absent.key));
        let nested = encode_file_node(&[absent.key]);
        let root = encode_file_node(&[nested.key]);
        store.put(root.key, &root.bytes).unwrap();
        assert!(matches!(verify_membership(&store, root.key, 8),
            Err(MembershipError::Missing { key }) if key == nested.key));
        store.put(nested.key, &nested.bytes).unwrap();
        assert!(matches!(verify_membership(&store, root.key, 8),
            Err(MembershipError::Missing { key }) if key == absent.key));
    }

    #[test]
    fn invalid_interiors_do_not_produce_evidence() {
        let (_directory, store) = store();
        let blob = encode_blob(b"leaf");
        let file = encode_file_node(&[blob.key]);
        store.put(file.key, b"wrong payload").unwrap();
        assert!(matches!(verify_membership(&store, file.key, 8),
            Err(MembershipError::InvalidInterior { key }) if key == file.key));
        let malformed = [0xff];
        let key = Key::new(Type::FileNode, 0, &malformed);
        store.put(key, &malformed).unwrap();
        assert!(matches!(
            verify_membership(&store, key, 8),
            Err(MembershipError::Children { .. })
        ));
        store.close().unwrap();
        assert!(verify_membership(&store, file.key, 8).is_err());
    }

    #[test]
    fn evidence_requires_sealing_and_does_not_pin_or_scrub_leaves() {
        let (_directory, store) = store();
        let blob = encode_blob(b"expected payload");
        let root = encode_file_node(&[blob.key]);
        store.put(blob.key, b"different payload").unwrap();
        store.put(root.key, &root.bytes).unwrap();
        let proof = verify_membership(&store, root.key, 8).unwrap();
        assert_eq!(proof.object_count(), 2);
        assert!(proof.into_sealed_membership().is_err());
        store.seal_snapshot().unwrap();
        let proof = verify_membership(&store, root.key, 8).unwrap();
        let membership = proof.into_sealed_membership().unwrap();
        store
            .compact(|_| false, packstore::CompactOpts::default())
            .unwrap();
        assert!(!store.has(root.key).unwrap());
        assert!(membership.contains(root.key));
    }
}
