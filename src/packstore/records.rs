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
    pub fn records_in_order(&self, keys: impl IntoIterator<Item = Key>) -> Result<Records, Error> {
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
