//! Free Space Map (FSM) for tracking available space in heap pages.
//!
//! The FSM maintains an approximate record of how much free space each heap
//! page has. This avoids scanning every page during inserts to find one with
//! enough room.
//!
//! The design follows `PostgreSQL`'s FSM approach: each page's free space is
//! encoded into a single byte (category), where each increment represents
//! approximately 32 bytes of free space. This makes the FSM very compact.
//!
//! ## Category encoding
//!
//! ```text
//!   category = min(255, free_space / FSM_CAT_STEP)
//!   free_space_approx = category * FSM_CAT_STEP
//! ```
//!
//! With `FSM_CAT_STEP = 32`, category 255 represents >= 8160 bytes free,
//! which covers the entire usable page.

#![allow(clippy::cast_possible_truncation)]

use parking_lot::Mutex;

/// Granularity of free space categories. Each category step represents
/// this many bytes of free space.
pub const FSM_CAT_STEP: usize = 32;

/// Maximum category value (a page with maximum free space).
pub const FSM_CAT_MAX: u8 = 255;

#[inline]
fn u64_to_usize(value: u64) -> Option<usize> {
    usize::try_from(value).ok()
}

/// Convert a byte count of free space to an FSM category.
#[must_use]
pub fn space_to_cat(free_space: usize) -> u8 {
    let cat = free_space / FSM_CAT_STEP;
    if cat > usize::from(FSM_CAT_MAX) {
        FSM_CAT_MAX
    } else {
        // cat is guaranteed <= 255 here, so the truncation is safe.
        u8::try_from(cat).unwrap_or(FSM_CAT_MAX)
    }
}

/// Convert an FSM category back to an approximate byte count.
#[must_use]
pub fn cat_to_space(cat: u8) -> usize {
    usize::from(cat) * FSM_CAT_STEP
}

/// Convert a desired tuple size (including the line pointer overhead) to the
/// minimum FSM category that can satisfy it.
#[must_use]
pub fn needed_cat(tuple_size_with_overhead: usize) -> u8 {
    // Round up to the next category.
    let cat = tuple_size_with_overhead.div_ceil(FSM_CAT_STEP);
    if cat > usize::from(FSM_CAT_MAX) {
        FSM_CAT_MAX
    } else {
        // cat is guaranteed <= 255 here, so the truncation is safe.
        u8::try_from(cat).unwrap_or(FSM_CAT_MAX)
    }
}

/// In-memory free space map for a single relation.
///
/// Stores one byte per page, encoding the approximate free space category.
/// Pages not yet tracked are assumed to be uninitialized (category 0 = no
/// free space).
///
/// The FSM is rebuilt from disk on recovery, so it does not
/// need its own durability.
#[derive(Debug)]
pub struct FreeSpaceMap {
    inner: Mutex<FsmInner>,
}

#[derive(Debug)]
struct FsmInner {
    /// One category byte per page. Index = page number.
    cats: Vec<u8>,
    /// Hint: the page number where the last successful allocation was found.
    /// The next search starts from here to spread inserts across pages.
    search_hint: usize,
}

impl FreeSpaceMap {
    /// Create a new, empty free space map.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(FsmInner {
                cats: Vec::new(),
                search_hint: 0,
            }),
        }
    }

    /// Create a free space map with a known number of pages.
    #[must_use]
    pub fn with_page_count(page_count: usize) -> Self {
        Self {
            inner: Mutex::new(FsmInner {
                cats: vec![0; page_count],
                search_hint: 0,
            }),
        }
    }

    /// Record the free space for a specific page.
    pub fn update(&self, page_number: u64, free_space: usize) {
        let mut inner = self.inner.lock();
        let Some(page_idx) = u64_to_usize(page_number) else {
            return;
        };
        if page_idx >= inner.cats.len() {
            inner.cats.resize(page_idx + 1, 0);
        }
        inner.cats[page_idx] = space_to_cat(free_space);
    }

    /// Find a page with at least `min_free_space` bytes of free space.
    ///
    /// Returns the page number, or `None` if no page has enough space.
    /// Uses a round-robin search starting from the last allocation hint.
    #[must_use]
    pub fn find_page(&self, min_free_space: usize) -> Option<u64> {
        let min_cat = needed_cat(min_free_space);
        let mut inner = self.inner.lock();
        let len = inner.cats.len();
        if len == 0 {
            return None;
        }

        let start = inner.search_hint % len;

        // Search from hint to end.
        for i in start..len {
            if inner.cats[i] >= min_cat {
                inner.search_hint = i + 1;
                return Some(i as u64);
            }
        }
        // Wrap around from beginning to hint.
        for i in 0..start {
            if inner.cats[i] >= min_cat {
                inner.search_hint = i + 1;
                return Some(i as u64);
            }
        }

        None
    }

    /// Number of pages tracked by this FSM.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.inner.lock().cats.len()
    }

    /// Mark a page as full (category 0).
    pub fn mark_full(&self, page_number: u64) {
        self.update(page_number, 0);
    }

    /// Remove tracking for a page (e.g. after truncation).
    pub fn remove(&self, page_number: u64) {
        let mut inner = self.inner.lock();
        let Some(idx) = u64_to_usize(page_number) else {
            return;
        };
        if idx < inner.cats.len() {
            inner.cats[idx] = 0;
        }
    }

    /// Return the approximate free space recorded for a page.
    #[must_use]
    pub fn get_free_space(&self, page_number: u64) -> usize {
        let inner = self.inner.lock();
        let Some(idx) = u64_to_usize(page_number) else {
            return 0;
        };
        if idx < inner.cats.len() {
            cat_to_space(inner.cats[idx])
        } else {
            0
        }
    }

    /// Reset the FSM, clearing all entries.
    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.cats.clear();
        inner.search_hint = 0;
    }

    /// Rebuild the FSM from an iterator of (`page_number`, `free_space`) pairs.
    pub fn rebuild(&self, entries: impl Iterator<Item = (u64, usize)>) {
        let mut inner = self.inner.lock();
        inner.cats.clear();
        inner.search_hint = 0;
        for (page_number, free_space) in entries {
            let Some(idx) = u64_to_usize(page_number) else {
                continue;
            };
            if idx >= inner.cats.len() {
                inner.cats.resize(idx + 1, 0);
            }
            inner.cats[idx] = space_to_cat(free_space);
        }
    }

    /// Total estimated free bytes across all tracked pages.
    #[must_use]
    pub fn total_free_space(&self) -> u64 {
        let inner = self.inner.lock();
        inner
            .cats
            .iter()
            .map(|&cat| u64::from(cat) * (FSM_CAT_STEP as u64))
            .sum()
    }
}

impl Default for FreeSpaceMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::PAGE_SIZE;

    #[test]
    fn space_to_cat_and_back() {
        assert_eq!(space_to_cat(0), 0);
        assert_eq!(space_to_cat(31), 0);
        assert_eq!(space_to_cat(32), 1);
        assert_eq!(space_to_cat(64), 2);
        assert_eq!(space_to_cat(PAGE_SIZE), FSM_CAT_MAX);

        assert_eq!(cat_to_space(0), 0);
        assert_eq!(cat_to_space(1), 32);
        assert_eq!(cat_to_space(FSM_CAT_MAX), 255 * 32);
    }

    #[test]
    fn needed_cat_rounds_up() {
        assert_eq!(needed_cat(0), 0);
        assert_eq!(needed_cat(1), 1);
        assert_eq!(needed_cat(32), 1);
        assert_eq!(needed_cat(33), 2);
        assert_eq!(needed_cat(64), 2);
    }

    #[test]
    fn find_page_empty_returns_none() {
        let fsm = FreeSpaceMap::new();
        assert!(fsm.find_page(100).is_none());
    }

    #[test]
    fn find_page_returns_page_with_enough_space() {
        let fsm = FreeSpaceMap::new();
        fsm.update(0, 100); // Small space.
        fsm.update(1, 4000); // Plenty of space.
        fsm.update(2, 200); // Medium space.

        let page = fsm.find_page(3000).unwrap();
        assert_eq!(page, 1);
    }

    #[test]
    fn find_page_wraps_around() {
        let fsm = FreeSpaceMap::new();
        fsm.update(0, 4000);
        fsm.update(1, 4000);
        fsm.update(2, 4000);

        // First find returns page 0.
        let p1 = fsm.find_page(100).unwrap();
        assert_eq!(p1, 0);

        // Second find should start from hint (1) and return page 1.
        let p2 = fsm.find_page(100).unwrap();
        assert_eq!(p2, 1);

        // Third find returns page 2.
        let p3 = fsm.find_page(100).unwrap();
        assert_eq!(p3, 2);

        // Fourth wraps around to page 0.
        let p4 = fsm.find_page(100).unwrap();
        assert_eq!(p4, 0);
    }

    #[test]
    fn mark_full_prevents_allocation() {
        let fsm = FreeSpaceMap::new();
        fsm.update(0, 4000);
        fsm.mark_full(0);
        assert!(fsm.find_page(100).is_none());
    }

    #[test]
    fn rebuild_replaces_all_entries() {
        let fsm = FreeSpaceMap::new();
        fsm.update(0, 4000);
        fsm.update(1, 4000);

        fsm.rebuild([(5, 2000), (10, 3000)].into_iter());
        assert_eq!(fsm.page_count(), 11);
        assert_eq!(fsm.get_free_space(0), 0); // Was cleared.
        assert!(fsm.get_free_space(5) >= 1984); // 2000 / 32 * 32 = 1984.
    }

    #[test]
    fn total_free_space_sums_all_pages() {
        let fsm = FreeSpaceMap::new();
        fsm.update(0, 1000);
        fsm.update(1, 2000);
        let total = fsm.total_free_space();
        // Due to categorization rounding, the total is approximate.
        assert!(total > 0);
    }

    #[test]
    fn with_page_count_initializes_zeroed() {
        let fsm = FreeSpaceMap::with_page_count(10);
        assert_eq!(fsm.page_count(), 10);
        assert!(fsm.find_page(1).is_none());
    }
}
