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
    /// Sealed payloads borrow their mappings; active reads reuse one buffer.
    /// An error can leave a copied prefix. Sync the destination before publishing references.
    pub fn copy_to_unflushed(self, destination: &Store) -> Result<(), Error> {
        let mut buffer = Vec::new();
        for location in self.locations {
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
            destination.put_record_unflushed(location.key, bytes)?;
        }
        Ok(())
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
                    if let Some((offset, stored_length)) = segment.locate_record(key) {
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
        let mut segments: Vec<_> = shared
            .sealed
            .iter()
            .map(|s| Segment::Sealed(Arc::clone(s)))
            .collect();
        if let Some(segment) = &shared.active {
            segments.push(Segment::Active(Arc::clone(segment)));
        }
        drop(active);
        drop(shared);
        locations.sort_unstable_by_key(|location| (location.segment, location.offset));
        Ok(Records {
            segments,
            locations: locations.into_iter(),
        })
    }
}
