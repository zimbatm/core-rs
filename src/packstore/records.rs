use std::os::unix::fs::FileExt;
use std::sync::Arc;

use super::{ActiveSegment, Error, Store, footer::SealedSegment, unpoison};
use crate::{amberpack::REC_HEADER_SIZE, key::Key};

enum Segment {
    Active(Arc<ActiveSegment>),
    Sealed(Arc<SealedSegment>),
}

struct Location {
    key: Key,
    segment: usize,
    offset: u64,
    stored_length: u32,
}

/// Encoded records from a captured set of segment locations, in physical order.
/// Segment handles remain alive across collection, wipe, and store closure.
/// Like `get_record`, reads do not check CRC or payload hashes; the receiver must
/// validate them before publication. Dropping this reader releases its handles.
pub struct Records {
    segments: Vec<Segment>,
    locations: std::vec::IntoIter<Location>,
}

/// A fixed set of encoded records with retained segment handles.
/// Keys outside the captured set return NotFound, even if the store has them.
/// Reads do not validate payloads; receivers must validate before publication.
/// Dropping the view releases its handles, including unlinked segment files.
pub struct RecordView {
    segments: Vec<Segment>,
    locations: std::collections::HashMap<Key, RecordLocation>,
}

struct RecordLocation {
    segment: usize,
    offset: u64,
    stored_length: u32,
}

pub(super) struct ValidatedRecord<'a> {
    key: Key,
    bytes: std::borrow::Cow<'a, [u8]>,
}

impl<'a> ValidatedRecord<'a> {
    pub(super) fn new(key: Key, bytes: std::borrow::Cow<'a, [u8]>) -> Result<Self, Error> {
        let record = crate::amberpack::parse_record(&bytes).map_err(Error::Pack)?;
        if record.key != key || bytes.len() != REC_HEADER_SIZE + record.slen as usize {
            return Err(Error::Verify(
                "encoded record key or length mismatch".into(),
            ));
        }
        let payload = &bytes[REC_HEADER_SIZE..];
        let data = if record.flags == 0 {
            std::borrow::Cow::Borrowed(payload)
        } else {
            std::borrow::Cow::Owned(
                super::decode_payload(record.flags, record.ulen, payload).map_err(Error::Pack)?,
            )
        };
        if Key::new(key.type_(), key.length(), &data) != key {
            return Err(Error::Verify(
                "encoded record payload checksum mismatch".into(),
            ));
        }
        drop(data);
        Ok(Self { key, bytes })
    }
}

impl Segment {
    fn validated_record(&self, location: &Location) -> Result<ValidatedRecord<'_>, Error> {
        let bytes = match self {
            Self::Active(segment) => {
                let mut bytes = vec![0; REC_HEADER_SIZE + location.stored_length as usize];
                segment.f.read_exact_at(&mut bytes, location.offset)?;
                std::borrow::Cow::Owned(bytes)
            }
            Self::Sealed(segment) => std::borrow::Cow::Borrowed(
                segment.record_bytes_at(location.offset, location.stored_length)?,
            ),
        };
        ValidatedRecord::new(location.key, bytes)
    }
}

impl Store {
    pub(super) fn put_validated_record(&self, record: ValidatedRecord<'_>) -> Result<(), Error> {
        {
            let shared = unpoison(self.shared.read());
            if shared.closed {
                return Err(Error::Closed);
            }
            if let Some(message) = &shared.failed {
                return Err(Error::Failed(message.clone()));
            }
        }
        // Dedup hits must remain visible to an active collection barrier.
        self.observe(record.key);
        if self.has(record.key)? {
            return Ok(());
        }
        self.append(record.key, &record.bytes, false)
    }
}

fn validate_batch<'a>(
    segments: &'a [Segment],
    locations: &[Location],
) -> Vec<Result<ValidatedRecord<'a>, Error>> {
    let mut records = Vec::with_capacity(locations.len());
    for location in locations {
        let record = segments[location.segment].validated_record(location);
        let failed = record.is_err();
        records.push(record);
        if failed {
            break;
        }
    }
    records
}

#[derive(Default)]
struct CopyBatch {
    bytes: Vec<u8>,
    index: std::collections::HashMap<Key, super::ActiveLoc>,
}

impl CopyBatch {
    fn stage(&mut self, record: &ValidatedRecord<'_>, active_offset: u64) -> u64 {
        let offset = active_offset + self.bytes.len() as u64;
        self.index.insert(
            record.key,
            super::ActiveLoc {
                off: offset,
                flags: record.bytes[33],
                ulen: super::be_u32(&record.bytes, 34),
                slen: super::be_u32(&record.bytes, 38),
            },
        );
        self.bytes.extend_from_slice(&record.bytes);
        offset + record.bytes.len() as u64
    }
}

impl Store {
    fn flush_copy_batch(
        &self,
        append: &mut super::AppendState,
        batch: &mut CopyBatch,
    ) -> Result<(), Error> {
        self.flush_copy_batch_with(append, batch, |file, bytes, offset| {
            file.write_all_at(bytes, offset)
        })
    }

    fn flush_copy_batch_with(
        &self,
        append: &mut super::AppendState,
        batch: &mut CopyBatch,
        write: impl FnOnce(&std::fs::File, &[u8], u64) -> std::io::Result<()>,
    ) -> Result<(), Error> {
        if batch.bytes.is_empty() {
            return Ok(());
        }
        let active = append
            .active
            .as_mut()
            .expect("buffered records have an active segment");
        if let Err(error) = write(&active.seg.f, &batch.bytes, active.size) {
            // A partial batch can contain complete, unindexed records. Reopen must recover it
            // before another writer can append over that prefix.
            self.set_failed(&error);
            return Err(error.into());
        }
        unpoison(active.seg.index.write()).extend(batch.index.drain());
        active.size += batch.bytes.len() as u64;
        batch.bytes.clear();
        Ok(())
    }

    fn put_validated_records(
        &self,
        records: Vec<Result<ValidatedRecord<'_>, Error>>,
        batch: &mut CopyBatch,
    ) -> Result<(), Error> {
        let mut append = self.append_lock();
        {
            let shared = unpoison(self.shared.read());
            if shared.closed {
                return Err(Error::Closed);
            }
            if let Some(message) = &shared.failed {
                return Err(Error::Failed(message.clone()));
            }
        }
        for result in records {
            let record = match result {
                Ok(record) => record,
                Err(error) => {
                    self.flush_copy_batch(&mut append, batch)?;
                    return Err(error);
                }
            };
            self.observe(record.key);
            if batch.index.contains_key(&record.key) {
                continue;
            }
            match self.has(record.key) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    self.flush_copy_batch(&mut append, batch)?;
                    return Err(error);
                }
            }
            // Oversized records already amortize a syscall; avoid an extra large allocation.
            if record.bytes.len() >= 1024 * 1024 {
                self.flush_copy_batch(&mut append, batch)?;
                self.append_locked(&mut append, record.key, &record.bytes, false)?;
                continue;
            }
            if append.active.is_none() {
                self.create_active(&mut append)?;
            }
            let end = batch.stage(
                &record,
                append.active.as_ref().expect("created active segment").size,
            );
            let rotate = end >= self.cfg.segment_size;
            if rotate || batch.bytes.len() >= 1024 * 1024 {
                self.flush_copy_batch(&mut append, batch)?;
            }
            if rotate {
                if let Err(error) = self.seal_active(&mut append) {
                    self.set_failed(&error);
                    return Err(error);
                }
            }
        }
        self.flush_copy_batch(&mut append, batch)
    }
}

impl RecordView {
    pub fn get_record(&self, key: Key) -> Result<Vec<u8>, Error> {
        let location = self.locations.get(&key).ok_or(Error::NotFound)?;
        match &self.segments[location.segment] {
            Segment::Active(segment) => {
                let mut bytes = vec![0; REC_HEADER_SIZE + location.stored_length as usize];
                segment.f.read_exact_at(&mut bytes, location.offset)?;
                Ok(bytes)
            }
            Segment::Sealed(segment) => segment.record_at(location.offset, location.stored_length),
        }
    }
}

impl Records {
    /// Converts captured locations into a random-access view without reading payloads.
    /// Duplicate keys share one location in the resulting view.
    pub fn into_view(self) -> RecordView {
        let mut indices = vec![usize::MAX; self.segments.len()];
        for location in self.locations.as_slice() {
            indices[location.segment] = 0;
        }
        let mut segments = Vec::new();
        for (index, segment) in self.segments.into_iter().enumerate() {
            if indices[index] != usize::MAX {
                indices[index] = segments.len();
                segments.push(segment);
            }
        }
        RecordView {
            segments,
            locations: self
                .locations
                .map(|location| {
                    (
                        location.key,
                        RecordLocation {
                            segment: indices[location.segment],
                            offset: location.offset,
                            stored_length: location.stored_length,
                        },
                    )
                })
                .collect(),
        }
    }

    /// Copies records through the destination's full record validation path.
    /// Sealed payloads borrow their mappings; bounded workers validate before serial writes.
    /// An error can leave a copied prefix. Sync the destination before publishing references.
    pub fn copy_to_unflushed(self, destination: &Store) -> Result<(), Error> {
        self.copy_with_workers(
            destination,
            std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(8),
        )
    }

    fn copy_with_workers(self, destination: &Store, workers: usize) -> Result<(), Error> {
        // One token protects the entire copied prefix from the GC write horizon.
        let _write_token = destination.begin_write();
        let locations = self.locations.as_slice();
        let mut batches = Vec::new();
        let mut start = 0;
        let mut bytes = 0;
        for (index, location) in locations.iter().enumerate() {
            bytes += REC_HEADER_SIZE + location.stored_length as usize;
            if bytes >= 1024 * 1024 || index + 1 - start >= 256 {
                batches.push(&locations[start..=index]);
                start = index + 1;
                bytes = 0;
            }
        }
        if start < locations.len() {
            batches.push(&locations[start..]);
        }
        let workers = workers.max(1).min(batches.len());
        if workers <= 1 {
            let mut output = CopyBatch::default();
            for batch in batches {
                destination
                    .put_validated_records(validate_batch(&self.segments, batch), &mut output)?;
            }
            return Ok(());
        }
        std::thread::scope(|scope| {
            let mut receivers = Vec::with_capacity(workers);
            for worker in 0..workers {
                let (sender, receiver) = std::sync::mpsc::sync_channel(0);
                receivers.push(receiver);
                let batches = &batches;
                let segments = &self.segments;
                scope.spawn(move || {
                    for batch in batches.iter().skip(worker).step_by(workers) {
                        let records = validate_batch(segments, batch);
                        let failed = records.last().is_some_and(Result::is_err);
                        if sender.send(records).is_err() || failed {
                            break;
                        }
                    }
                });
            }
            // Rendezvous channels bound validation ahead of the serial writer.
            // Dropping every receiver on error also releases blocked workers.
            let mut output = CopyBatch::default();
            for index in 0..batches.len() {
                let records = receivers[index % workers]
                    .recv()
                    .expect("record validator panicked");
                destination.put_validated_records(records, &mut output)?;
            }
            Ok(())
        })
    }
}

impl Iterator for Records {
    type Item = Result<(Key, Vec<u8>), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let location = self.locations.next()?;
        let bytes = match &self.segments[location.segment] {
            Segment::Active(segment) => {
                let mut bytes = vec![0; REC_HEADER_SIZE + location.stored_length as usize];
                segment
                    .f
                    .read_exact_at(&mut bytes, location.offset)
                    .map(|()| bytes)
                    .map_err(Error::from)
            }
            Segment::Sealed(segment) => segment.record_at(location.offset, location.stored_length),
        };
        Some(bytes.map(|bytes| (location.key, bytes)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.locations.size_hint()
    }
}

impl ExactSizeIterator for Records {}

impl Store {
    /// Resolves each key once and retains its location for subsequent reads.
    /// Rejects missing keys and closed stores before returning a reader.
    /// Duplicate input keys produce duplicate records.
    /// Materialized input prevents caller iterator code from running under store locks.
    pub fn records_in_order(&self, keys: Vec<Key>) -> Result<Records, Error> {
        let shared = unpoison(self.shared.read());
        if shared.closed {
            return Err(Error::Closed);
        }
        let active = shared
            .active
            .as_ref()
            .map(|segment| unpoison(segment.index.read()));
        let mut locations = Vec::with_capacity(keys.len());
        let mut pending = std::collections::HashMap::<Key, usize>::with_capacity(keys.len());
        for key in keys {
            *pending.entry(key).or_default() += 1;
        }
        let mut record = |key, count, segment, offset, stored_length| {
            locations.extend((0..count).map(|_| Location {
                key,
                segment,
                offset,
                stored_length,
            }));
        };
        if let Some(active) = &active {
            pending.retain(|key, count| {
                if let Some(location) = active.get(key) {
                    record(
                        *key,
                        *count,
                        shared.sealed.len(),
                        location.off,
                        location.slen,
                    );
                    false
                } else {
                    true
                }
            });
        }
        for (index, segment) in shared.sealed.iter().enumerate().rev() {
            if pending.is_empty() {
                break;
            }
            let segment = segment.load()?;
            let entries = segment.index_entries();
            if pending.len() >= entries.len() {
                for entry in entries {
                    if let Some(count) = pending.remove(&entry.k) {
                        record(entry.k, count, index, entry.off, entry.slen);
                    }
                    if pending.is_empty() {
                        break;
                    }
                }
            } else {
                // HashMap iteration visits capacity; discard empty buckets before sparse lookup.
                pending.shrink_to_fit();
                pending.retain(|key, count| {
                    if let Some((offset, stored_length)) = segment.locate_record(*key) {
                        record(*key, *count, index, offset, stored_length);
                        false
                    } else {
                        true
                    }
                });
            }
        }
        if !pending.is_empty() {
            return Err(Error::NotFound);
        }
        drop(pending);
        locations.sort_unstable_by_key(|location| (location.segment, location.offset));
        let mut segments = Vec::new();
        let mut previous = None;
        for location in &mut locations {
            if previous != Some(location.segment) {
                let segment = if location.segment == shared.sealed.len() {
                    Segment::Active(Arc::clone(
                        shared.active.as_ref().expect("located active record"),
                    ))
                } else {
                    Segment::Sealed(Arc::clone(shared.sealed[location.segment].load()?))
                };
                segments.push(segment);
                previous = Some(location.segment);
            }
            location.segment = segments.len() - 1;
        }
        drop(active);
        drop(shared);
        Ok(Records {
            segments,
            locations: locations.into_iter(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Options, testutil::blob_obj};
    use super::*;
    use tempfile::TempDir;

    fn populate(store: &Store) -> Vec<Key> {
        (0..800u64)
            .map(|index| {
                let mut data = vec![b'x'; 2048];
                data.extend_from_slice(&index.to_le_bytes());
                let object = blob_obj(&data);
                store.put_unflushed(object.key, &object.data).unwrap();
                object.key
            })
            .collect()
    }

    #[test]
    fn bulk_lookup_preserves_newest_records_for_dense_and_sparse_requests() {
        use super::super::testutil::write_sealed_file;
        let objects: Vec<_> = (0..4u8).map(|n| blob_obj(&[n; 32])).collect();
        let (directory, first, entries) = write_sealed_file(&objects);
        let second = directory.path().join("0000000000000002.seg");
        let mut bytes = std::fs::read(&first).unwrap();
        bytes[entries[0].off as usize + REC_HEADER_SIZE] ^= 1;
        std::fs::write(&second, bytes).unwrap();
        let store = Store::open(directory.path()).unwrap();
        for mut keys in [
            objects.iter().map(|object| object.key).collect::<Vec<_>>(),
            vec![objects[0].key],
        ] {
            keys.extend_from_within(..);
            let mut expected: Vec<_> = keys
                .iter()
                .map(|key| (*key, store.get_record(*key).unwrap()))
                .collect();
            expected.sort_unstable_by_key(|(key, _)| *key);
            let mut actual = store
                .records_in_order(keys)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            actual.sort_unstable_by_key(|(key, _)| *key);
            assert_eq!(actual, expected);
        }
        let mut keys: Vec<_> = objects.iter().map(|object| object.key).collect();
        keys.push(blob_obj(b"missing").key);
        assert!(matches!(store.records_in_order(keys), Err(Error::NotFound)));
        let key = objects[0].key;
        let mut newest = store.get_record(key).unwrap();
        newest[REC_HEADER_SIZE] ^= 2;
        store.append(key, &newest, false).unwrap();
        let records = store
            .records_in_order(vec![key, key])
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(records, vec![(key, newest.clone()), (key, newest)]);
    }

    #[test]
    fn batched_copy_matches_record_writes_and_observes_dedup() {
        use super::super::testutil::incompressible;
        let source_dir = TempDir::new().unwrap();
        let source = Store::open(source_dir.path()).unwrap();
        let mut keys = populate(&source);
        let large = blob_obj(&incompressible(1024 * 1024 + 17));
        source.put_unflushed(large.key, &large.data).unwrap();
        keys.push(large.key);
        keys.push(keys[17]);
        let reference_dir = TempDir::new().unwrap();
        let options = Options::default().segment_size(8192);
        let reference = Store::open_with(reference_dir.path(), options).unwrap();
        reference
            .put_record_unflushed(keys[17], &source.get_record(keys[17]).unwrap())
            .unwrap();
        for record in source.records_in_order(keys.clone()).unwrap() {
            let (key, bytes) = record.unwrap();
            reference.put_record_unflushed(key, &bytes).unwrap();
        }
        let layout = |store: &Store| {
            store
                .segments()
                .unwrap()
                .into_iter()
                .map(|segment| (segment.body, segment.keys))
                .collect::<Vec<_>>()
        };
        for workers in [1, 3] {
            let target_dir = TempDir::new().unwrap();
            let target = Store::open_with(target_dir.path(), options).unwrap();
            target
                .put_record_unflushed(keys[17], &source.get_record(keys[17]).unwrap())
                .unwrap();
            target.begin_barrier();
            source
                .records_in_order(keys.clone())
                .unwrap()
                .copy_with_workers(&target, workers)
                .unwrap();
            assert_eq!(target.take_grey().unwrap(), keys.iter().copied().collect());
            assert_eq!(layout(&target), layout(&reference));
            target.sync().unwrap();
            target.close().unwrap();
            let target = Store::open(target_dir.path()).unwrap();
            for key in &keys {
                assert_eq!(
                    target.get_record(*key).unwrap(),
                    reference.get_record(*key).unwrap()
                );
            }
        }
    }

    #[test]
    fn partial_batch_write_requires_reopen_before_more_writes() {
        use crate::amberpack::encode_record;
        let directory = TempDir::new().unwrap();
        let store = Store::open(directory.path()).unwrap();
        let first = blob_obj(b"complete first record");
        let second = blob_obj(b"unfinished second record");
        let first_bytes = encode_record(first.key, &first.data).unwrap();
        let second_bytes = encode_record(second.key, &second.data).unwrap();
        let mut batch = CopyBatch::default();
        let mut append = store.append_lock();
        store.create_active(&mut append).unwrap();
        let offset = append.active.as_ref().unwrap().size;
        for (key, bytes) in [(first.key, &first_bytes), (second.key, &second_bytes)] {
            let record = ValidatedRecord::new(key, std::borrow::Cow::Borrowed(bytes)).unwrap();
            batch.stage(&record, offset);
        }
        let result = store.flush_copy_batch_with(&mut append, &mut batch, |file, bytes, offset| {
            file.write_all_at(&bytes[..first_bytes.len() + 5], offset)?;
            Err(std::io::Error::other("injected partial write"))
        });
        assert!(result.is_err());
        assert!(!store.has(first.key).unwrap());
        drop(append);
        assert!(matches!(
            store.put_unflushed(second.key, &second.data),
            Err(Error::Failed(_))
        ));
        let _ = store.close();
        drop(store);
        let recovered = Store::open(directory.path()).unwrap();
        assert_eq!(recovered.get(first.key).unwrap(), first.data);
        assert!(matches!(recovered.get(second.key), Err(Error::NotFound)));
        recovered.put_unflushed(second.key, &second.data).unwrap();
    }

    #[test]
    fn parallel_copy_preserves_encoding_and_retained_handles() {
        let source_dir = TempDir::new().unwrap();
        let source =
            Store::open_with(source_dir.path(), Options::default().segment_size(8192)).unwrap();
        let mut keys = populate(&source);
        keys.push(keys[17]);
        let expected = source
            .records_in_order(keys.clone())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let records = source.records_in_order(keys).unwrap();
        assert!(
            records
                .segments
                .iter()
                .any(|s| matches!(s, Segment::Sealed(_)))
        );
        source.wipe().unwrap();
        source.close().unwrap();
        let target_dir = TempDir::new().unwrap();
        let target =
            Store::open_with(target_dir.path(), Options::default().segment_size(4096)).unwrap();
        records.copy_with_workers(&target, 3).unwrap();
        target.sync().unwrap();
        target.close().unwrap();
        let target = Store::open(target_dir.path()).unwrap();
        for (key, bytes) in expected {
            assert_eq!(target.get_record(key).unwrap(), bytes);
        }
    }

    #[test]
    fn parallel_copy_stops_at_corrupt_record_even_on_dedup() {
        for workers in [1, 3] {
            let source_dir = TempDir::new().unwrap();
            let source = Store::open(source_dir.path()).unwrap();
            let keys = populate(&source);
            let records = source.records_in_order(keys.clone()).unwrap();
            let bad = &records.locations.as_slice()[300];
            let target_dir = TempDir::new().unwrap();
            let target = Store::open(target_dir.path()).unwrap();
            target
                .put_record_unflushed(bad.key, &source.get_record(bad.key).unwrap())
                .unwrap();
            let Segment::Active(segment) = &records.segments[bad.segment] else {
                panic!("expected active source");
            };
            let mut byte = [0];
            let offset = bad.offset + REC_HEADER_SIZE as u64;
            segment.f.read_exact_at(&mut byte, offset).unwrap();
            byte[0] ^= 1;
            segment.f.write_all_at(&byte, offset).unwrap();
            assert!(records.copy_with_workers(&target, workers).is_err());
            for key in &keys[..=300] {
                assert!(target.has(*key).unwrap());
            }
            for key in &keys[301..] {
                assert!(!target.has(*key).unwrap());
            }
        }
    }

    #[test]
    fn parallel_copy_releases_workers_on_destination_error() {
        let source_dir = TempDir::new().unwrap();
        let source = Store::open(source_dir.path()).unwrap();
        let keys = populate(&source);
        let target_dir = TempDir::new().unwrap();
        let target = Store::open(target_dir.path()).unwrap();
        target.close().unwrap();
        assert!(matches!(
            source
                .records_in_order(keys)
                .unwrap()
                .copy_with_workers(&target, 3),
            Err(Error::Closed)
        ));
    }
}
