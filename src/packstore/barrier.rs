//! Write-barrier grey capture: keys observed by the write paths between
//! [`Store::begin_barrier`] and the next [`Store::compact`] are treated as
//! live for that compact, letting ingests run concurrently with a collector's
//! mark (Go: `packstore/barrier.go`; see `specs/gc.qnt` in the Go repo).

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use crate::key::Key;

use super::{Store, unpoison};

impl Store {
    /// Starts grey capture: keys a write observes are live for the next
    /// [`Store::compact`], letting ingests run concurrently with the caller's
    /// mark. Compact itself must still not overlap ingests. Beginning a
    /// barrier on an already-capturing store just replaces the set (Go:
    /// `BeginBarrier`).
    pub fn begin_barrier(&self) {
        let mut grey = unpoison(self.grey.lock());
        *grey = Some(HashSet::new());
        self.capturing.store(true, Ordering::SeqCst);
    }

    /// Discards a capture without compacting (Go: `AbortBarrier`).
    pub fn abort_barrier(&self) {
        let mut grey = unpoison(self.grey.lock());
        self.capturing.store(false, Ordering::SeqCst);
        *grey = None;
    }

    /// Records one observed key in the grey set, if a capture is running.
    /// The `capturing` load is a lock-free fast path; the `Option` is
    /// re-checked under the lock, so an abort racing an observe is safe (Go:
    /// `observe`).
    pub(crate) fn observe(&self, k: Key) {
        if !self.capturing.load(Ordering::SeqCst) {
            return;
        }
        let mut grey = unpoison(self.grey.lock());
        if let Some(g) = grey.as_mut() {
            g.insert(k);
        }
    }

    /// Marks `keys` live for the next [`Store::compact`], like the write
    /// paths' per-key observation. A reference PUT that lands while a mark is
    /// running calls it with the root's whole closure, so a reference
    /// committed during the cycle never dangles (Go: `ObserveKeys`).
    pub fn observe_keys(&self, keys: &[Key]) {
        if !self.capturing.load(Ordering::SeqCst) {
            return;
        }
        let mut grey = unpoison(self.grey.lock());
        let Some(g) = grey.as_mut() else {
            return;
        };
        for &k in keys {
            g.insert(k);
        }
    }

    /// Ends the capture and hands the set to [`Store::compact`] (Go:
    /// `takeGrey`).
    pub(crate) fn take_grey(&self) -> Option<HashSet<Key>> {
        let mut grey = unpoison(self.grey.lock());
        self.capturing.store(false, Ordering::SeqCst);
        grey.take()
    }
}
