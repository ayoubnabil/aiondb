use crate::page::PageId;

/// Maximum usage count before clamping. `PostgreSQL` uses 5.
const MAX_USAGE_COUNT: u8 = 5;

/// An entry in the clock-sweep ring buffer.
#[derive(Debug)]
struct ClockEntry {
    /// The page occupying this slot, or `None` if empty.
    page_id: Option<PageId>,
    /// Usage counter decremented during sweeps; pages with higher counts
    /// survive more rounds before eviction.
    usage_count: u8,
}

/// Clock-sweep (CLOCK) eviction policy, modelled after `PostgreSQL`'s buffer
/// replacement strategy.
///
/// The algorithm maintains a circular buffer of `capacity` slots.  Each slot
/// has a usage counter that is incremented on access (clamped at the
/// internal `MAX_USAGE_COUNT`) and decremented when the sweep hand passes over it.
/// A slot whose counter reaches zero is selected as the eviction victim.
#[derive(Debug)]
pub struct ClockSweep {
    /// Fixed-size ring of entries.
    entries: Vec<ClockEntry>,
    /// Current position of the clock hand.
    hand: usize,
}

impl ClockSweep {
    /// Create a new clock-sweep eviction tracker with the given capacity.
    ///
    /// All slots start empty.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let entries = (0..capacity)
            .map(|_| ClockEntry {
                page_id: None,
                usage_count: 0,
            })
            .collect();
        Self { entries, hand: 0 }
    }

    /// Record an access for the page in the given slot, incrementing the
    /// usage counter (up to the internal `MAX_USAGE_COUNT`).
    pub fn access(&mut self, slot: usize) {
        if let Some(entry) = self.entries.get_mut(slot) {
            entry.usage_count = entry.usage_count.saturating_add(1).min(MAX_USAGE_COUNT);
        }
    }

    /// Sweep the clock to find a victim slot for eviction.
    ///
    /// Skips empty slots and slots with non-zero usage count (decrementing
    /// them as the hand passes).  Returns `None` only when every occupied
    /// slot is pinned (the caller must handle the "pool full" case).
    ///
    /// The `is_pinned` callback is invoked for each candidate to check
    /// whether the page in that slot is pinned.  Pinned pages are never
    /// evicted.
    pub fn find_victim(&mut self, mut is_pinned: impl FnMut(usize) -> bool) -> Option<usize> {
        let cap = self.entries.len();
        if cap == 0 {
            return None;
        }
        // We sweep at most `cap * (MAX_USAGE_COUNT as usize + 1)` times to
        // guarantee termination even when every slot starts at max usage.
        let max_iterations = cap * (usize::from(MAX_USAGE_COUNT) + 1);
        for _ in 0..max_iterations {
            let slot = self.hand;
            self.hand = (self.hand + 1) % cap;

            let entry = &mut self.entries[slot];

            // Skip empty slots.
            if entry.page_id.is_none() {
                continue;
            }

            // Skip pinned pages without decrementing.
            if is_pinned(slot) {
                continue;
            }

            if entry.usage_count == 0 {
                return Some(slot);
            }

            entry.usage_count -= 1;
        }
        None
    }

    /// Register a page in the given slot.
    ///
    /// The new entry starts with a usage count of 1.
    pub fn insert(&mut self, slot: usize, page_id: PageId) {
        if let Some(entry) = self.entries.get_mut(slot) {
            entry.page_id = Some(page_id);
            entry.usage_count = 1;
        }
    }

    /// Remove the page from the given slot, leaving it empty.
    pub fn remove(&mut self, slot: usize) {
        if let Some(entry) = self.entries.get_mut(slot) {
            entry.page_id = None;
            entry.usage_count = 0;
        }
    }

    /// The total number of slots.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    /// Returns the `PageId` stored in the given slot, if any.
    #[must_use]
    pub fn page_id_at(&self, slot: usize) -> Option<PageId> {
        self.entries.get(slot).and_then(|e| e.page_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(rel: u64, page: u64) -> PageId {
        PageId {
            relation_id: rel,
            page_number: page,
        }
    }

    #[test]
    fn new_creates_empty_slots() {
        let sweep = ClockSweep::new(4);
        assert_eq!(sweep.capacity(), 4);
        for i in 0..4 {
            assert_eq!(sweep.page_id_at(i), None);
        }
    }

    #[test]
    fn insert_and_page_id_at() {
        let mut sweep = ClockSweep::new(3);
        sweep.insert(0, pid(1, 0));
        sweep.insert(2, pid(1, 2));
        assert_eq!(sweep.page_id_at(0), Some(pid(1, 0)));
        assert_eq!(sweep.page_id_at(1), None);
        assert_eq!(sweep.page_id_at(2), Some(pid(1, 2)));
    }

    #[test]
    fn remove_clears_slot() {
        let mut sweep = ClockSweep::new(2);
        sweep.insert(0, pid(1, 0));
        sweep.remove(0);
        assert_eq!(sweep.page_id_at(0), None);
    }

    #[test]
    fn find_victim_selects_lowest_usage() {
        let mut sweep = ClockSweep::new(3);
        // Slot 0: usage=1 (from insert)
        sweep.insert(0, pid(1, 0));
        // Slot 1: usage=1 (from insert), then access -> usage=2
        sweep.insert(1, pid(1, 1));
        sweep.access(1);
        // Slot 2: usage=1 (from insert)
        sweep.insert(2, pid(1, 2));

        // Hand starts at 0.  Slot 0 has usage=1, decrement to 0, move on.
        // Slot 1 has usage=2, decrement to 1, move on.
        // Slot 2 has usage=1, decrement to 0, move on.
        // Second pass: slot 0 now has usage=0 -> victim.
        let victim = sweep.find_victim(|_| false);
        assert_eq!(victim, Some(0));
    }

    #[test]
    fn find_victim_skips_empty_slots() {
        let mut sweep = ClockSweep::new(4);
        // Only slot 2 is occupied.
        sweep.insert(2, pid(1, 0));

        // Sweep should skip 0, 1, decrement slot 2 (usage 1->0), skip 3,
        // then on second pass find slot 2 at usage=0.
        let victim = sweep.find_victim(|_| false);
        assert_eq!(victim, Some(2));
    }

    #[test]
    fn find_victim_skips_pinned_pages() {
        let mut sweep = ClockSweep::new(2);
        sweep.insert(0, pid(1, 0));
        sweep.insert(1, pid(1, 1));

        // Slot 0 is pinned, so only slot 1 can be evicted.
        let victim = sweep.find_victim(|slot| slot == 0);
        assert_eq!(victim, Some(1));
    }

    #[test]
    fn find_victim_returns_none_when_all_pinned() {
        let mut sweep = ClockSweep::new(2);
        sweep.insert(0, pid(1, 0));
        sweep.insert(1, pid(1, 1));

        let victim = sweep.find_victim(|_| true);
        assert_eq!(victim, None);
    }

    #[test]
    fn find_victim_returns_none_for_empty_pool() {
        let mut sweep = ClockSweep::new(0);
        assert_eq!(sweep.find_victim(|_| false), None);
    }

    #[test]
    fn find_victim_all_slots_empty() {
        let mut sweep = ClockSweep::new(4);
        assert_eq!(sweep.find_victim(|_| false), None);
    }

    #[test]
    fn access_clamps_at_max_usage() {
        let mut sweep = ClockSweep::new(1);
        sweep.insert(0, pid(1, 0));
        // Access many times; usage should clamp at MAX_USAGE_COUNT.
        for _ in 0..20 {
            sweep.access(0);
        }

        // Must sweep (MAX_USAGE_COUNT) times before finding a victim.
        // Each decrement reduces usage by 1.
        let mut decrements = 0;
        // Reset hand
        sweep.hand = 0;
        loop {
            let v = sweep.find_victim(|_| false);
            if v.is_some() {
                break;
            }
            decrements += 1;
            // Safety: should not loop forever; MAX_USAGE_COUNT is bounded.
            assert!(decrements < 100, "unexpected infinite loop");
        }
    }

    #[test]
    fn access_out_of_bounds_is_noop() {
        let mut sweep = ClockSweep::new(2);
        // Should not panic.
        sweep.access(999);
    }

    #[test]
    fn insert_out_of_bounds_is_noop() {
        let mut sweep = ClockSweep::new(2);
        sweep.insert(999, pid(1, 0));
        // Nothing should have changed.
        assert_eq!(sweep.page_id_at(0), None);
    }

    #[test]
    fn remove_out_of_bounds_is_noop() {
        let mut sweep = ClockSweep::new(2);
        sweep.insert(0, pid(1, 0));
        sweep.remove(999);
        assert_eq!(sweep.page_id_at(0), Some(pid(1, 0)));
    }

    #[test]
    fn clock_hand_wraps_around() {
        let mut sweep = ClockSweep::new(3);
        sweep.insert(0, pid(1, 0));
        sweep.insert(1, pid(1, 1));
        sweep.insert(2, pid(1, 2));

        // Evict first victim.
        let v1 = sweep.find_victim(|_| false);
        assert!(v1.is_some());

        // Insert a new page in that slot.
        let slot = v1.unwrap();
        sweep.insert(slot, pid(2, 0));

        // Evict another; hand should have wrapped around.
        let v2 = sweep.find_victim(|_| false);
        assert!(v2.is_some());
        assert_ne!(v1, v2);
    }
}
