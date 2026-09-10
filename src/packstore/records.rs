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
            let mut buffer = Vec::new();
            for location in locations {
                let bytes = match &self.segments[location.segment] {
                    Segment::Active(segment) => {
                        buffer.resize(REC_HEADER_SIZE + location.stored_length as usize, 0);
                        segment.f.read_exact_at(&mut buffer, location.offset)?;
                        &buffer[..]
                    }
                    Segment::Sealed(segment) => {
                        segment.record_bytes_at(location.offset, location.stored_length)?
                    }
                };
                destination.put_validated_record(ValidatedRecord::new(
                    location.key,
                    std::borrow::Cow::Borrowed(bytes),
                )?)?;
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
                        let mut records = Vec::with_capacity(batch.len());
                        let mut failed = false;
                        for location in *batch {
                            let record = segments[location.segment].validated_record(location);
                            failed = record.is_err();
                            records.push(record);
                            if failed {
                                break;
                            }
                        }
                        if sender.send(records).is_err() || failed {
                            break;
                        }
                    }
                });
            }
            // Rendezvous channels bound validation ahead of the serial writer.
            // Dropping every receiver on error also releases blocked workers.
            for index in 0..batches.len() {
                let records = receivers[index % workers]
                    .recv()
                    .expect("record validator panicked");
                for record in records {
                    destination.put_validated_record(record?)?;
                }
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
        let keys = keys.into_iter();
        let mut locations = Vec::with_capacity(keys.size_hint().0);
        for key in keys {
            let locate = || {
                if let Some(location) = active.as_ref().and_then(|index| index.get(&key)) {
                    return Ok(Location {
                        key,
                        segment: shared.sealed.len(),
                        offset: location.off,
                        stored_length: location.slen,
                    });
                }
                for (index, segment) in shared.sealed.iter().enumerate().rev() {
                    if let Some((offset, stored_length)) = segment.load()?.locate_record(key) {
                        return Ok(Location {
                            key,
                            segment: index,
                            offset,
                            stored_length,
                        });
                    }
                }
                Err(Error::NotFound)
            };
            locations.push(locate()?);
        }
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
