//! A liveness mark over a store snapshot: one bit per sealed record, slotted
//! by footer index position, plus a map for the active segment (Go:
//! `packstore/markset.go`).

use std::collections::HashMap;
use std::sync::Arc;

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

/// Where [`MarkSet::locate`] found a key.
enum Loc {
    Active,
    Sealed { seg: usize, pos: usize },
}

impl Store {
    /// Snapshots the store into a fresh, unmarked [`MarkSet`]: every sealed
    /// segment (ascending id, one bitmap each) plus the keys of the active
    /// segment's in-RAM index (Go: `NewMarkSet`).
    pub fn new_mark_set(&self) -> MarkSet {
        let sh = unpoison(self.shared.read());
        let mut m = MarkSet {
            segs: Vec::with_capacity(sh.sealed.len()),
            bits: Vec::with_capacity(sh.sealed.len()),
            active: HashMap::new(),
            marked: 0,
        };
        for g in &sh.sealed {
            m.segs.push(g.clone());
            m.bits
                .push(vec![0u64; g.fv.key_count.div_ceil(64) as usize]);
        }
        if let Some(a) = &sh.active {
            for k in unpoison(a.index.read()).keys() {
                m.active.insert(*k, false);
            }
        }
        m
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
