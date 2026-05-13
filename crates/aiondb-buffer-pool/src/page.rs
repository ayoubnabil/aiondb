/// Fixed-size page: 8 KiB, matching `PostgreSQL`'s default block size.
pub const PAGE_SIZE: usize = 8192;

/// Unique identifier for a page in the buffer pool.
///
/// Combines a relation (table) identifier with a page number to form a
/// globally unique address for every page in the database.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageId {
    /// The table/relation this page belongs to.
    pub relation_id: u64,
    /// The page number within the relation.
    pub page_number: u64,
}

impl std::fmt::Display for PageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {})", self.relation_id, self.page_number)
    }
}

/// A fixed-size page of data held in the buffer pool.
///
/// Each page tracks whether it has been modified (`dirty`) and how many
/// active references exist (`pin_count`). A page with a non-zero pin count
/// must not be evicted.
#[derive(Debug)]
pub struct Page {
    /// Logical address of this page.
    pub id: PageId,
    /// Raw page data.
    data: Box<[u8; PAGE_SIZE]>,
    /// Whether the page has been modified since the last flush.
    dirty: bool,
    /// Whether this dirty page has already been counted in the pool-level
    /// dirty counter for the current dirty epoch. Cleared by `mark_clean`.
    /// Prevents multiple concurrent writers from each incrementing the
    /// counter on the clean -> dirty transition (audit buffer-pool F1).
    dirty_counted: bool,
    /// Number of active references holding this page in the pool.
    pin_count: u32,
}

impl Page {
    /// Create a new, zero-filled page with the given identifier.
    #[must_use]
    pub fn new(id: PageId) -> Self {
        Self {
            id,
            data: Box::new([0u8; PAGE_SIZE]),
            dirty: false,
            dirty_counted: false,
            pin_count: 0,
        }
    }

    /// Create a page pre-filled with the given data.
    #[must_use]
    pub fn with_data(id: PageId, data: [u8; PAGE_SIZE]) -> Self {
        Self {
            id,
            data: Box::new(data),
            dirty: false,
            dirty_counted: false,
            pin_count: 0,
        }
    }

    /// Immutable access to the raw page data.
    #[must_use]
    pub fn data(&self) -> &[u8; PAGE_SIZE] {
        &self.data
    }

    /// Mutable access to the raw page data.
    ///
    /// Automatically marks the page as dirty.
    pub fn data_mut(&mut self) -> &mut [u8; PAGE_SIZE] {
        self.dirty = true;
        &mut self.data
    }

    /// Returns `true` if the page has been modified since the last flush.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Reset the dirty flag (typically after flushing to disk).
    pub fn mark_clean(&mut self) {
        self.dirty = false;
        self.dirty_counted = false;
    }

    /// Atomically mark this page as dirty-counted in the pool counter and
    /// report whether this call performed the transition. The caller must
    /// hold the frame write lock so two writers cannot race on the same flip.
    pub fn claim_dirty_transition(&mut self) -> bool {
        if self.dirty && !self.dirty_counted {
            self.dirty_counted = true;
            true
        } else {
            false
        }
    }

    /// Increment the pin count, preventing eviction.
    pub fn pin(&mut self) {
        self.pin_count = self.pin_count.saturating_add(1);
    }

    /// Decrement the pin count.  Uses saturating subtraction so an extra
    /// unpin never underflows.
    pub fn unpin(&mut self) {
        self.pin_count = self.pin_count.saturating_sub(1);
    }

    /// Returns `true` if the page is currently pinned.
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.pin_count > 0
    }

    /// Current pin count (useful for diagnostics).
    #[must_use]
    pub fn pin_count(&self) -> u32 {
        self.pin_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_id() -> PageId {
        PageId {
            relation_id: 1,
            page_number: 0,
        }
    }

    #[test]
    fn new_page_is_zeroed_clean_and_unpinned() {
        let page = Page::new(sample_id());
        assert_eq!(page.id, sample_id());
        assert!(!page.is_dirty());
        assert!(!page.is_pinned());
        assert_eq!(page.pin_count(), 0);
        assert!(page.data().iter().all(|&b| b == 0));
    }

    #[test]
    fn with_data_preserves_content() {
        let mut raw = [0u8; PAGE_SIZE];
        raw[0] = 0xDE;
        raw[PAGE_SIZE - 1] = 0xAD;
        let page = Page::with_data(sample_id(), raw);
        assert_eq!(page.data()[0], 0xDE);
        assert_eq!(page.data()[PAGE_SIZE - 1], 0xAD);
        assert!(!page.is_dirty());
    }

    #[test]
    fn data_mut_marks_page_dirty() {
        let mut page = Page::new(sample_id());
        assert!(!page.is_dirty());
        page.data_mut()[42] = 0xFF;
        assert!(page.is_dirty());
        assert_eq!(page.data()[42], 0xFF);
    }

    #[test]
    fn mark_clean_resets_dirty_flag() {
        let mut page = Page::new(sample_id());
        page.data_mut()[0] = 1;
        assert!(page.is_dirty());
        page.mark_clean();
        assert!(!page.is_dirty());
    }

    #[test]
    fn pin_and_unpin_tracking() {
        let mut page = Page::new(sample_id());
        assert!(!page.is_pinned());

        page.pin();
        assert!(page.is_pinned());
        assert_eq!(page.pin_count(), 1);

        page.pin();
        assert_eq!(page.pin_count(), 2);

        page.unpin();
        assert_eq!(page.pin_count(), 1);
        assert!(page.is_pinned());

        page.unpin();
        assert_eq!(page.pin_count(), 0);
        assert!(!page.is_pinned());
    }

    #[test]
    fn unpin_saturates_at_zero() {
        let mut page = Page::new(sample_id());
        page.unpin();
        assert_eq!(page.pin_count(), 0);
        page.unpin();
        assert_eq!(page.pin_count(), 0);
    }

    #[test]
    fn page_id_display() {
        let id = PageId {
            relation_id: 5,
            page_number: 42,
        };
        assert_eq!(id.to_string(), "(5, 42)");
    }

    #[test]
    fn page_id_ordering() {
        let a = PageId {
            relation_id: 1,
            page_number: 2,
        };
        let b = PageId {
            relation_id: 1,
            page_number: 3,
        };
        let c = PageId {
            relation_id: 2,
            page_number: 0,
        };
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn page_id_hash_and_eq() {
        use std::collections::HashSet;
        let id1 = PageId {
            relation_id: 1,
            page_number: 1,
        };
        let id2 = PageId {
            relation_id: 1,
            page_number: 1,
        };
        let id3 = PageId {
            relation_id: 1,
            page_number: 2,
        };
        let mut set = HashSet::new();
        set.insert(id1);
        assert!(set.contains(&id2));
        assert!(!set.contains(&id3));
    }
}
