//! A liveness mark over a store snapshot: one bit per sealed record, slotted
//! by footer index position, plus a map for the active segment (Go:
//! `packstore/markset.go`).

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
};

use crate::key::Key;

use super::footer::{SealedSegment, filter_key};
use super::{Store, unpoison};

/// A liveness mark over a snapshot: one bit per sealed record, slotted by
/// footer index, plus a map for the active segment. Marking requires `&mut`
/// (Go: "not concurrency-safe; run it under the writer quiesce GC requires"),
/// but the set holds `Arc` snapshots of the sealed segments and a copy of the
/// active index's keys — not a borrow of the [`Store`] — so a mark walk is
/// safe to run concurrently with ingests: it touches only its own snapshot
/// plus the immutable sealed footers (Go: `MarkSet`).
pub struct MarkSet {
    segs: Vec<Arc<SealedSegment>>,
    bits: Vec<Vec<u64>>,
    /// Present in the active segment; the value is "marked".
    active: HashMap<Key, bool>,
    /// Keys marked so far.
    marked: usize,
}

/// Concurrent membership over captured records. This does not verify content or retain it.
/// Consume this value after all marking threads finish to recover an ordinary mark set.
pub struct ConcurrentMarkSet {
    segs: Vec<Arc<SealedSegment>>,
    bits: Vec<Vec<AtomicU64>>,
    active: HashMap<Key, AtomicBool>,
}

impl MarkSet {
    pub fn into_concurrent(self) -> ConcurrentMarkSet {
        ConcurrentMarkSet {
            segs: self.segs,
            bits: self
                .bits
                .into_iter()
                .map(|words| words.into_iter().map(AtomicU64::new).collect())
                .collect(),
            active: self
                .active
                .into_iter()
                .map(|(key, marked)| (key, AtomicBool::new(marked)))
                .collect(),
        }
    }
}

impl ConcurrentMarkSet {
    /// Exactly one caller receives `newly = true` for each previously unmarked key.
    /// Relaxed ordering only arbitrates work; callers must join workers before using their results.
    pub fn mark(&self, key: Key) -> (bool, bool) {
        if let Some(flag) = self.active.get(&key) {
            if flag.load(Relaxed) {
                return (false, true);
            }
            return (!flag.swap(true, Relaxed), true);
        }
        for (index, segment) in self.segs.iter().enumerate().rev() {
            if !segment.fv.filter_contains(&segment.mm, filter_key(key)) {
                continue;
            }
            if let Some(position) = segment.fv.lookup_pos(&segment.mm, key) {
                let word = &self.bits[index][position / 64];
                let mask = 1u64 << (position % 64);
                if word.load(Relaxed) & mask != 0 {
                    return (false, true);
                }
                return (word.fetch_or(mask, Relaxed) & mask == 0, true);
            }
        }
        (false, false)
    }

    /// Ownership prevents conversion while scoped marking threads still borrow this value.
    /// The result has the same snapshot and retention limits as `MarkSet`.
    pub fn into_mark_set(self) -> MarkSet {
        let bits: Vec<Vec<u64>> = self
            .bits
            .into_iter()
            .map(|words| words.into_iter().map(AtomicU64::into_inner).collect())
            .collect();
        let active: HashMap<Key, bool> = self
            .active
            .into_iter()
            .map(|(key, flag)| (key, flag.into_inner()))
            .collect();
        let marked = bits
            .iter()
            .flatten()
            .map(|word| word.count_ones() as usize)
            .sum::<usize>()
            + active.values().filter(|marked| **marked).count();
        MarkSet {
            segs: self.segs,
            bits,
            active,
            marked,
        }
    }
}

/// Where [`MarkSet::locate`] found a key.
enum Loc {
    Active,
    Sealed { seg: usize, pos: usize },
}

impl Store {
    /// Snapshots the store into a fresh, unmarked [`MarkSet`]: every sealed
    /// segment (ascending id, one bitmap each) plus the keys of the active
    /// segment's in-RAM index (Go: `NewMarkSet`).
    /// Initializes all captured indexes before exposing infallible marking.
    /// Returns an error if any deferred index cannot be initialized.
    pub fn new_mark_set(&self) -> Result<MarkSet, super::Error> {
        let sh = unpoison(self.shared.read());
        let mut m = MarkSet {
            segs: Vec::with_capacity(sh.sealed.len()),
            bits: Vec::with_capacity(sh.sealed.len()),
            active: HashMap::new(),
            marked: 0,
        };
        for g in &sh.sealed {
            let g = g.load()?;
            m.segs.push(g.clone());
            m.bits
                .push(vec![0u64; g.fv.key_count.div_ceil(64) as usize]);
        }
        if let Some(a) = &sh.active {
            for k in unpoison(a.index.read()).keys() {
                m.active.insert(*k, false);
            }
        }
        Ok(m)
    }
}

impl MarkSet {
    /// Finds `k` newest-first, matching the read path: the active map first,
    /// then the sealed segments from newest to oldest (filter skip, then
    /// exact index position) (Go: `locate`).
    fn locate(&self, k: Key) -> Option<Loc> {
        if self.active.contains_key(&k) {
            return Some(Loc::Active);
        }
        for (i, g) in self.segs.iter().enumerate().rev() {
            if !g.fv.filter_contains(&g.mm, filter_key(k)) {
                continue;
            }
            if let Some(pos) = g.fv.lookup_pos(&g.mm, k) {
                return Some(Loc::Sealed { seg: i, pos });
            }
        }
        None
    }

    /// Marks `k`, reporting `(newly, present)`: whether it was unmarked
    /// before, and whether it is present in the snapshot at all (Go: `Mark`).
    pub fn mark(&mut self, k: Key) -> (bool, bool) {
        match self.locate(k) {
            None => (false, false),
            Some(Loc::Active) => {
                let slot = self.active.get_mut(&k).expect("located in active");
                let newly = !*slot;
                *slot = true;
                if newly {
                    self.marked += 1;
                }
                (newly, true)
            }
            Some(Loc::Sealed { seg, pos }) => {
                let w = &mut self.bits[seg][pos / 64];
                let b = 1u64 << (pos % 64);
                let newly = *w & b == 0;
                *w |= b;
                if newly {
                    self.marked += 1;
                }
                (newly, true)
            }
        }
    }

    /// Reports whether `k` has been marked (Go: `Contains`).
    pub fn contains(&self, k: Key) -> bool {
        match self.locate(k) {
            None => false,
            Some(Loc::Active) => self.active[&k],
            Some(Loc::Sealed { seg, pos }) => self.bits[seg][pos / 64] & (1 << (pos % 64)) != 0,
        }
    }

    pub(super) fn contains_record(
        &self,
        segment: &SealedSegment,
        position: usize,
        key: Key,
    ) -> bool {
        if let Ok(index) = self.segs.binary_search_by_key(&segment.id, |g| g.id)
            && std::ptr::eq(self.segs[index].as_ref(), segment)
            && self.bits[index]
                .get(position / 64)
                .is_some_and(|word| word & (1 << (position % 64)) != 0)
        {
            return true;
        }
        // An older duplicate or a segment sealed after capture can still contain a live key.
        self.contains(key)
    }

    /// Returns the number of distinct keys marked so far (Go: `Marked`).
    pub fn marked(&self) -> usize {
        self.marked
    }
}

/// Untrusted serialized membership for one immutable segment.
/// Bit positions refer to its exact footer layout. Callers must authenticate
/// both these values and the segment bytes before relying on membership.
#[derive(Clone, Debug)]
pub struct SegmentBitmap {
    pub segment_id: u64,
    pub record_count: u64,
    pub words: Vec<u64>,
}

/// Exact membership in captured sealed records, independent of GC liveness.
/// This does not prove reachability, completeness, or current store presence.
pub struct SealedMembership {
    marks: MarkSet,
}

impl MarkSet {
    /// Consumes marks only when their snapshot has no active records.
    /// Seal the store before creating the mark set used for persistence.
    pub fn into_sealed_membership(self) -> Result<SealedMembership, super::Error> {
        if !self.active.is_empty() {
            return Err(super::corrupt(
                "membership snapshot contains active records",
            ));
        }
        Ok(SealedMembership { marks: self })
    }
}

impl SealedMembership {
    pub fn contains(&self, key: Key) -> bool {
        self.marks.contains(key)
    }

    /// Copies portable bitmap values for caller-defined authenticated encoding.
    pub fn bitmaps(&self) -> Vec<SegmentBitmap> {
        self.marks
            .segs
            .iter()
            .zip(&self.marks.bits)
            .map(|(segment, words)| SegmentBitmap {
                segment_id: segment.id,
                record_count: segment.fv.key_count,
                words: words.clone(),
            })
            .collect()
    }
}

impl super::SegmentSnapshot {
    /// Restores exact membership against this capture's footer layouts.
    /// IDs must be strictly increasing; counts, lengths, and padding must match.
    /// This validates structure only. The caller must authenticate the bitmap
    /// and each referenced segment's bytes. Extra captured segments are excluded.
    /// Retained captures do not establish current store presence after collection.
    pub fn restore_membership(
        &self,
        bitmaps: &[SegmentBitmap],
    ) -> Result<SealedMembership, super::Error> {
        let mut marks = MarkSet {
            segs: Vec::with_capacity(bitmaps.len()),
            bits: Vec::with_capacity(bitmaps.len()),
            active: HashMap::new(),
            marked: 0,
        };
        let mut previous = None;
        for bitmap in bitmaps {
            if previous.is_some_and(|id| id >= bitmap.segment_id) {
                return Err(super::corrupt("membership segment order"));
            }
            previous = Some(bitmap.segment_id);
            let index = self
                .segments
                .binary_search_by_key(&bitmap.segment_id, |s| s.id)
                .map_err(|_| super::corrupt("missing membership segment"))?;
            let segment = &self.segments[index];
            if bitmap.record_count != segment.fv.key_count
                || bitmap.words.len() as u64 != bitmap.record_count.div_ceil(64)
            {
                return Err(super::corrupt("membership footer layout mismatch"));
            }
            let remainder = bitmap.record_count % 64;
            if remainder != 0
                && bitmap
                    .words
                    .last()
                    .is_some_and(|word| word >> remainder != 0)
            {
                return Err(super::corrupt("membership padding is nonzero"));
            }
            marks.segs.push(segment.clone());
            marks.bits.push(bitmap.words.clone());
        }
        Ok(SealedMembership { marks })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::Type;
    use tempfile::TempDir;

    #[test]
    fn concurrent_marks_preserve_claims_and_exact_physical_bits() {
        use std::sync::Barrier;
        let directory = TempDir::new().unwrap();
        let store =
            Store::open_with(directory.path(), super::super::Options::new().sync(false)).unwrap();
        let duplicate = Key::new(Type::Blob, 9, b"duplicate");
        let record = super::super::encode_record(duplicate, b"duplicate").unwrap();
        let mut keys = vec![duplicate];
        for segment in 0u8..4 {
            for index in 0u8..17 {
                let data = [segment, index];
                let key = Key::new(Type::Blob, data.len() as u64, &data);
                store.put(key, &data).unwrap();
                keys.push(key);
            }
            store.append(duplicate, &record, false).unwrap();
            store.seal_snapshot().unwrap();
        }
        store.append(duplicate, &record, false).unwrap();
        for index in 0u8..5 {
            let data = [99, index];
            let key = Key::new(Type::Blob, data.len() as u64, &data);
            store.put(key, &data).unwrap();
            keys.push(key);
        }
        let absent = Key::new(Type::Blob, 6, b"absent");
        for sealed in [false, true] {
            if sealed {
                store.seal_snapshot().unwrap();
            }
            for jobs in [1, 2, 8] {
                for duplicate_queries in [false, true] {
                    let mut scalar = store.new_mark_set().unwrap();
                    let mut before = store.new_mark_set().unwrap();
                    scalar.mark(keys[1]);
                    before.mark(keys[1]);
                    let shared = before.into_concurrent();
                    let barrier = Barrier::new(jobs);
                    let newly = std::thread::scope(|scope| {
                        let workers: Vec<_> = (0..jobs)
                            .map(|worker| {
                                let shared = &shared;
                                let keys = &keys;
                                let barrier = &barrier;
                                scope.spawn(move || {
                                    barrier.wait();
                                    let mut claims = 0;
                                    for index in 0..keys.len() {
                                        if duplicate_queries || index % jobs == worker {
                                            let key = if duplicate_queries {
                                                keys[(index + worker) % keys.len()]
                                            } else {
                                                keys[index]
                                            };
                                            let (newly, present) = shared.mark(key);
                                            assert!(present);
                                            claims += usize::from(newly);
                                        }
                                    }
                                    assert_eq!(shared.mark(absent), (false, false));
                                    claims
                                })
                            })
                            .collect();
                        workers
                            .into_iter()
                            .map(|worker| worker.join().unwrap())
                            .sum::<usize>()
                    });
                    let frozen = shared.into_mark_set();
                    for key in &keys {
                        scalar.mark(*key);
                    }
                    assert_eq!(newly, keys.len() - 1);
                    assert_eq!(frozen.marked, scalar.marked);
                    assert_eq!(frozen.bits, scalar.bits);
                    assert_eq!(frozen.active, scalar.active);
                    assert_eq!(frozen.marked(), keys.len());
                    assert!(!frozen.contains(absent));
                }
            }
        }
    }

    #[test]
    fn indexed_mark_requires_the_captured_segment_identity() {
        let first_dir = TempDir::new().unwrap();
        let second_dir = TempDir::new().unwrap();
        let first = Store::open(first_dir.path()).unwrap();
        let second = Store::open(second_dir.path()).unwrap();
        let a = Key::new(Type::Blob, 1, b"a");
        let b = Key::new(Type::Blob, 1, b"b");
        first.put(a, b"a").unwrap();
        second.put(b, b"b").unwrap();
        let first_snapshot = first.seal_snapshot().unwrap();
        let second_snapshot = second.seal_snapshot().unwrap();
        assert_eq!(
            first_snapshot.segments[0].id,
            second_snapshot.segments[0].id
        );
        let mut marks = first.new_mark_set().unwrap();
        assert_eq!(marks.mark(a), (true, true));
        assert!(marks.contains_record(&first_snapshot.segments[0], 0, a));
        assert!(!marks.contains_record(&second_snapshot.segments[0], 0, b));
    }
}
