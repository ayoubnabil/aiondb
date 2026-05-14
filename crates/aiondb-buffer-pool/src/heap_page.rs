//! Heap page layout for storing variable-length tuples within fixed-size pages.
//!
//! The layout mirrors `PostgreSQL`'s heap page design:
//!
//! ```text
//!  +---------------------------+
//!  | PageHeader (32 bytes)     |
//!  +---------------------------+
//!  | ItemId array (4 bytes ea) |  <-- grows downward from header
//!  +---------------------------+
//!  |       free space          |
//!  +---------------------------+
//!  | tuple data                |  <-- grows upward from end of page
//!  +---------------------------+
//! ```
//!
//! ## Page header (32 bytes)
//!
//! | Offset | Size | Field          | Description                              |
//! |--------|------|----------------|------------------------------------------|
//! |      0 |    8 | `magic`        | `b"AIONHP01"` page type identifier       |
//! |      8 |    4 | `lower`        | Byte offset to start of free space       |
//! |     12 |    4 | `upper`        | Byte offset to end of free space         |
//! |     16 |    8 | `page_lsn`     | LSN of last modification (for WAL)       |
//! |     24 |    2 | `item_count`   | Number of line pointers (`ItemId` entries)  |
//! |     26 |    2 | `flags`        | Page flags (reserved)                    |
//! |     28 |    4 | `_reserved`    | Alignment padding / future use           |
//!
//! ## `ItemId` (line pointer) - 4 bytes each
//!
//! | Bits  | Field     | Description                         |
//! |-------|-----------|-------------------------------------|
//! | 0-14  | `offset`  | Byte offset of tuple data in page   |
//! | 15    | `_pad`    | Reserved                            |
//! | 16-29 | `length`  | Byte length of tuple data           |
//! | 30-31 | `flags`   | 0 = unused, 1 = normal, 2 = dead   |

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::must_use_candidate
)]

use crate::page::PAGE_SIZE;

/// Magic bytes identifying a heap page.
pub const HEAP_PAGE_MAGIC: &[u8; 8] = b"AIONHP01";

/// Size of the page header in bytes.
pub const PAGE_HEADER_SIZE: usize = 32;

/// Size of each `ItemId` (line pointer) entry in bytes.
pub const ITEM_ID_SIZE: usize = 4;

/// Maximum usable space per page for tuple data and line pointers.
pub const MAX_USABLE_SPACE: usize = PAGE_SIZE - PAGE_HEADER_SIZE;

/// Maximum tuple size that can fit in a single page (leaving room for one
/// line pointer and the header).
pub const MAX_TUPLE_SIZE: usize = PAGE_SIZE - PAGE_HEADER_SIZE - ITEM_ID_SIZE;

#[inline]
fn u32_to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[inline]
fn usize_to_u32(value: usize) -> Option<u32> {
    u32::try_from(value).ok()
}

#[inline]
fn usize_to_u16(value: usize) -> Option<u16> {
    u16::try_from(value).ok()
}

// --- ItemId flags ---

/// The slot is unused (available for reuse).
pub const ITEM_UNUSED: u8 = 0;
/// The slot contains a normal (live) tuple.
pub const ITEM_NORMAL: u8 = 1;
/// The slot contains a dead tuple (deleted, pending vacuum).
pub const ITEM_DEAD: u8 = 2;
/// The slot has been redirected (HOT chain).
pub const ITEM_REDIRECT: u8 = 3;

// --- Page flags ---

/// The page has no special flags.
pub const PAGE_FLAG_NONE: u16 = 0;
/// The page is full (hint to avoid inserting).
pub const PAGE_FLAG_FULL: u16 = 1;

/// Packed line pointer (`ItemId`) stored in 4 bytes.
///
/// Layout:
/// - bits  0..14: offset (15 bits, max 32767 - sufficient for 8K pages)
/// - bit  15:     reserved
/// - bits 16..29: length (14 bits, max 16383 - sufficient for max tuple)
/// - bits 30..31: flags (2 bits)
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemId(u32);

impl ItemId {
    /// Create a new `ItemId` with the given offset, length, and flags.
    ///
    /// # Panics
    /// Panics in debug builds if offset >= 32768 or length >= 16384.
    #[must_use]
    pub fn new(offset: u16, length: u16, flags: u8) -> Self {
        debug_assert!(offset < (1 << 15), "offset must fit in 15 bits");
        debug_assert!(length < (1 << 14), "length must fit in 14 bits");
        debug_assert!(flags < 4, "flags must fit in 2 bits");
        let packed = u32::from(offset & 0x7FFF)
            | (u32::from(length & 0x3FFF) << 16)
            | (u32::from(flags & 0x03) << 30);
        Self(packed)
    }

    /// Create an unused (empty) `ItemId`.
    #[must_use]
    pub const fn unused() -> Self {
        Self(0)
    }

    /// Byte offset of the tuple data within the page.
    #[must_use]
    pub fn offset(self) -> u16 {
        (self.0 & 0x7FFF) as u16
    }

    /// Byte length of the tuple data.
    #[must_use]
    pub fn length(self) -> u16 {
        ((self.0 >> 16) & 0x3FFF) as u16
    }

    /// Status flags for this line pointer.
    #[must_use]
    pub fn flags(self) -> u8 {
        ((self.0 >> 30) & 0x03) as u8
    }

    /// Returns true if this slot is unused.
    #[must_use]
    pub fn is_unused(self) -> bool {
        self.flags() == ITEM_UNUSED
    }

    /// Returns true if this slot contains a live tuple.
    #[must_use]
    pub fn is_normal(self) -> bool {
        self.flags() == ITEM_NORMAL
    }

    /// Returns true if this slot contains a dead tuple.
    #[must_use]
    pub fn is_dead(self) -> bool {
        self.flags() == ITEM_DEAD
    }

    /// Encode to little-endian bytes.
    #[must_use]
    pub fn to_le_bytes(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }

    /// Decode from little-endian bytes.
    #[must_use]
    pub fn from_le_bytes(bytes: [u8; 4]) -> Self {
        Self(u32::from_le_bytes(bytes))
    }
}

/// Read/write interface for a heap page.
///
/// This is a zero-copy view over a mutable `[u8; PAGE_SIZE]` buffer.
pub struct HeapPage<'a> {
    data: &'a mut [u8; PAGE_SIZE],
}

/// Errors returned by [`HeapPage::compact`] when the page contents are not
/// safe to rewrite.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeapPageCompactError {
    CorruptedTuple {
        slot_index: u16,
        offset: u16,
        length: u16,
    },
    UnsupportedItem {
        slot_index: u16,
        flags: u8,
    },
}

impl std::fmt::Display for HeapPageCompactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CorruptedTuple {
                slot_index,
                offset,
                length,
            } => write!(
                f,
                "heap page tuple at slot {slot_index} is corrupt (offset={offset}, length={length})"
            ),
            Self::UnsupportedItem { slot_index, flags } => write!(
                f,
                "heap page slot {slot_index} uses unsupported item flags {flags}"
            ),
        }
    }
}

impl std::error::Error for HeapPageCompactError {}

impl<'a> HeapPage<'a> {
    /// Interpret the given buffer as a heap page.
    ///
    /// Does **not** validate the magic; the caller must check
    /// [`Self::is_initialized`] or call [`Self::init`] first.
    pub fn from_buf(data: &'a mut [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    /// Initialize a fresh, empty heap page.
    pub fn init(&mut self) {
        self.data.fill(0);
        self.data[..HEAP_PAGE_MAGIC.len()].copy_from_slice(HEAP_PAGE_MAGIC);
        self.set_lower(usize_to_u32(PAGE_HEADER_SIZE).unwrap_or(u32::MAX));
        self.set_upper(usize_to_u32(PAGE_SIZE).unwrap_or(u32::MAX));
        self.set_item_count(0);
        self.set_flags(PAGE_FLAG_NONE);
    }

    /// Returns true if this page has been initialized with the heap page magic.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.data[..HEAP_PAGE_MAGIC.len()] == *HEAP_PAGE_MAGIC
    }

    // --- Header field accessors ---

    /// Start of free space (end of the line pointer array).
    #[must_use]
    pub fn lower(&self) -> u32 {
        u32::from_le_bytes([self.data[8], self.data[9], self.data[10], self.data[11]])
    }

    fn set_lower(&mut self, value: u32) {
        self.data[8..12].copy_from_slice(&value.to_le_bytes());
    }

    /// End of free space (start of tuple data area).
    #[must_use]
    pub fn upper(&self) -> u32 {
        u32::from_le_bytes([self.data[12], self.data[13], self.data[14], self.data[15]])
    }

    fn set_upper(&mut self, value: u32) {
        self.data[12..16].copy_from_slice(&value.to_le_bytes());
    }

    /// LSN of the last modification to this page.
    #[must_use]
    pub fn page_lsn(&self) -> u64 {
        u64::from_le_bytes([
            self.data[16],
            self.data[17],
            self.data[18],
            self.data[19],
            self.data[20],
            self.data[21],
            self.data[22],
            self.data[23],
        ])
    }

    /// Set the page LSN.
    pub fn set_page_lsn(&mut self, lsn: u64) {
        self.data[16..24].copy_from_slice(&lsn.to_le_bytes());
    }

    /// Number of line pointers (`ItemId` entries) on this page.
    #[must_use]
    pub fn item_count(&self) -> u16 {
        u16::from_le_bytes([self.data[24], self.data[25]])
    }

    fn set_item_count(&mut self, count: u16) {
        self.data[24..26].copy_from_slice(&count.to_le_bytes());
    }

    /// Page flags.
    #[must_use]
    pub fn flags(&self) -> u16 {
        u16::from_le_bytes([self.data[26], self.data[27]])
    }

    /// Set page flags.
    pub fn set_flags(&mut self, flags: u16) {
        self.data[26..28].copy_from_slice(&flags.to_le_bytes());
    }

    // --- Free space ---

    /// Amount of free space available for new tuples (including the line pointer).
    #[must_use]
    pub fn free_space(&self) -> usize {
        let lower = u32_to_usize(self.lower());
        let upper = u32_to_usize(self.upper());
        upper.saturating_sub(lower)
    }

    /// Returns true if the page can fit a tuple of the given size.
    #[must_use]
    pub fn can_fit(&self, tuple_size: usize) -> bool {
        self.free_space() >= tuple_size + ITEM_ID_SIZE
    }

    // --- Line pointer (ItemId) access ---

    /// Read the `ItemId` at the given slot index (0-based).
    ///
    /// Returns `None` if the index is out of range.
    #[must_use]
    pub fn item_id(&self, index: u16) -> Option<ItemId> {
        if index >= self.item_count() {
            return None;
        }
        let offset = PAGE_HEADER_SIZE + usize::from(index) * ITEM_ID_SIZE;
        if offset + ITEM_ID_SIZE > PAGE_SIZE {
            return None;
        }
        let bytes: [u8; 4] = [
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
        ];
        Some(ItemId::from_le_bytes(bytes))
    }

    /// Write an `ItemId` at the given slot index.
    fn set_item_id(&mut self, index: u16, item_id: ItemId) {
        let offset = PAGE_HEADER_SIZE + usize::from(index) * ITEM_ID_SIZE;
        self.data[offset..offset + ITEM_ID_SIZE].copy_from_slice(&item_id.to_le_bytes());
    }

    // --- Tuple data access ---

    /// Read tuple data at the given slot index.
    ///
    /// Returns `None` if the slot is unused or out of range.
    #[must_use]
    pub fn read_tuple(&self, index: u16) -> Option<&[u8]> {
        let item = self.item_id(index)?;
        if !item.is_normal() {
            return None;
        }
        let start = usize::from(item.offset());
        let end = start.saturating_add(usize::from(item.length()));
        if end > PAGE_SIZE {
            return None;
        }
        Some(&self.data[start..end])
    }

    /// Insert a tuple into this page.
    ///
    /// Returns the slot index (0-based) of the inserted tuple, or `None` if
    /// there is not enough space.
    pub fn insert_tuple(&mut self, tuple_data: &[u8]) -> Option<u16> {
        let tuple_len = tuple_data.len();
        if tuple_len == 0 || tuple_len > MAX_TUPLE_SIZE {
            return None;
        }

        // Check if we can reuse an unused slot.
        let reuse_slot = self.find_unused_slot();
        let need_new_slot = reuse_slot.is_none();
        let required_space = if need_new_slot {
            tuple_len + ITEM_ID_SIZE
        } else {
            tuple_len
        };

        if self.free_space() < required_space {
            return None;
        }

        // Allocate space at the end of the page (upper grows downward).
        let new_upper = u32_to_usize(self.upper()).saturating_sub(tuple_len);
        self.data[new_upper..new_upper + tuple_len].copy_from_slice(tuple_data);
        self.set_upper(usize_to_u32(new_upper)?);

        let item = ItemId::new(
            usize_to_u16(new_upper)?,
            usize_to_u16(tuple_len)?,
            ITEM_NORMAL,
        );

        if let Some(slot) = reuse_slot {
            self.set_item_id(slot, item);
            Some(slot)
        } else {
            let slot = self.item_count();
            self.set_item_id(slot, item);
            self.set_item_count(slot + 1);
            let next_lower = PAGE_HEADER_SIZE + (usize::from(slot) + 1) * ITEM_ID_SIZE;
            self.set_lower(usize_to_u32(next_lower)?);
            Some(slot)
        }
    }

    /// Mark a tuple slot as dead (logically deleted).
    ///
    /// Returns `true` if the slot was successfully marked, `false` if the
    /// slot was already dead/unused or out of range.
    pub fn mark_dead(&mut self, index: u16) -> bool {
        let Some(item) = self.item_id(index) else {
            return false;
        };
        if !item.is_normal() {
            return false;
        }
        let dead = ItemId::new(item.offset(), item.length(), ITEM_DEAD);
        self.set_item_id(index, dead);
        true
    }

    /// Compact the page by removing dead tuples and reclaiming space.
    ///
    /// Preserves slot indices so external tuple references remain valid.
    ///
    /// Returns the number of dead tuples removed.
    pub fn compact(&mut self) -> Result<u16, HeapPageCompactError> {
        let count = self.item_count();
        if count == 0 {
            return Ok(0);
        }

        let mut live_tuples: Vec<Option<Vec<u8>>> = vec![None; usize::from(count)];
        let mut dead_removed = 0u16;

        for i in 0..count {
            let item = self
                .item_id(i)
                .ok_or(HeapPageCompactError::CorruptedTuple {
                    slot_index: i,
                    offset: 0,
                    length: 0,
                })?;
            if item.is_normal() {
                let start = usize::from(item.offset());
                let end = start.saturating_add(usize::from(item.length()));
                if end > PAGE_SIZE {
                    return Err(HeapPageCompactError::CorruptedTuple {
                        slot_index: i,
                        offset: item.offset(),
                        length: item.length(),
                    });
                }
                live_tuples[usize::from(i)] = Some(self.data[start..end].to_vec());
            } else if item.is_dead() {
                dead_removed += 1;
            } else if !item.is_unused() {
                return Err(HeapPageCompactError::UnsupportedItem {
                    slot_index: i,
                    flags: item.flags(),
                });
            }
        }

        if dead_removed == 0 {
            return Ok(0);
        }

        // Rebuild the page in a temporary buffer so compaction is atomic: any
        // corruption detected above leaves the original page untouched.
        let page_lsn = self.page_lsn();
        let lower = PAGE_HEADER_SIZE + usize::from(count) * ITEM_ID_SIZE;
        let mut rebuilt = [0u8; PAGE_SIZE];
        let mut compacted = HeapPage::from_buf(&mut rebuilt);
        compacted.init();
        compacted.set_page_lsn(page_lsn);
        compacted.set_flags(self.flags() & !PAGE_FLAG_FULL);
        compacted.set_item_count(count);
        compacted.set_lower(usize_to_u32(lower).unwrap_or(u32::MAX));

        let mut upper = PAGE_SIZE;
        for (slot, tuple_data) in live_tuples.into_iter().enumerate().rev() {
            let Some(tuple_data) = tuple_data else {
                continue;
            };
            upper = upper.checked_sub(tuple_data.len()).ok_or(
                HeapPageCompactError::CorruptedTuple {
                    slot_index: u16::try_from(slot).unwrap_or(u16::MAX),
                    offset: 0,
                    length: usize_to_u16(tuple_data.len()).unwrap_or(u16::MAX),
                },
            )?;
            if upper < lower {
                return Err(HeapPageCompactError::CorruptedTuple {
                    slot_index: u16::try_from(slot).unwrap_or(u16::MAX),
                    offset: 0,
                    length: usize_to_u16(tuple_data.len()).unwrap_or(u16::MAX),
                });
            }
            compacted.data[upper..upper + tuple_data.len()].copy_from_slice(&tuple_data);
            compacted.set_item_id(
                u16::try_from(slot).unwrap_or(u16::MAX),
                ItemId::new(
                    usize_to_u16(upper).unwrap_or(u16::MAX),
                    usize_to_u16(tuple_data.len()).unwrap_or(u16::MAX),
                    ITEM_NORMAL,
                ),
            );
        }
        compacted.set_upper(usize_to_u32(upper).unwrap_or(u32::MAX));
        self.data.copy_from_slice(&rebuilt);

        Ok(dead_removed)
    }

    /// Find the first unused slot, if any.
    fn find_unused_slot(&self) -> Option<u16> {
        for i in 0..self.item_count() {
            if let Some(item) = self.item_id(i) {
                if item.is_unused() {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Count of live (normal) tuples on this page.
    #[must_use]
    pub fn live_tuple_count(&self) -> u16 {
        let mut count = 0u16;
        for i in 0..self.item_count() {
            if let Some(item) = self.item_id(i) {
                if item.is_normal() {
                    count += 1;
                }
            }
        }
        count
    }

    /// Count of dead tuples on this page.
    #[must_use]
    pub fn dead_tuple_count(&self) -> u16 {
        let mut count = 0u16;
        for i in 0..self.item_count() {
            if let Some(item) = self.item_id(i) {
                if item.is_dead() {
                    count += 1;
                }
            }
        }
        count
    }
}

// --- Read-only view ---

/// Read-only view over a heap page.
pub struct HeapPageRef<'a> {
    data: &'a [u8; PAGE_SIZE],
}

impl<'a> HeapPageRef<'a> {
    /// Interpret the given buffer as a read-only heap page.
    pub fn from_buf(data: &'a [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    /// Returns true if this page has been initialized.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.data[..HEAP_PAGE_MAGIC.len()] == *HEAP_PAGE_MAGIC
    }

    /// Number of line pointers.
    #[must_use]
    pub fn item_count(&self) -> u16 {
        u16::from_le_bytes([self.data[24], self.data[25]])
    }

    /// Free space in bytes.
    #[must_use]
    pub fn free_space(&self) -> usize {
        let lower = u32_to_usize(u32::from_le_bytes([
            self.data[8],
            self.data[9],
            self.data[10],
            self.data[11],
        ]));
        let upper = u32_to_usize(u32::from_le_bytes([
            self.data[12],
            self.data[13],
            self.data[14],
            self.data[15],
        ]));
        upper.saturating_sub(lower)
    }

    /// Read an `ItemId` at the given slot index.
    #[must_use]
    pub fn item_id(&self, index: u16) -> Option<ItemId> {
        if index >= self.item_count() {
            return None;
        }
        let offset = PAGE_HEADER_SIZE + usize::from(index) * ITEM_ID_SIZE;
        if offset + ITEM_ID_SIZE > PAGE_SIZE {
            return None;
        }
        let bytes: [u8; 4] = [
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
        ];
        Some(ItemId::from_le_bytes(bytes))
    }

    /// Read tuple data at the given slot.
    #[must_use]
    pub fn read_tuple(&self, index: u16) -> Option<&[u8]> {
        let item = self.item_id(index)?;
        if !item.is_normal() {
            return None;
        }
        let start = usize::from(item.offset());
        let end = start.saturating_add(usize::from(item.length()));
        if end > PAGE_SIZE {
            return None;
        }
        Some(&self.data[start..end])
    }

    /// Count dead tuples on the page.
    #[must_use]
    pub fn dead_tuple_count(&self) -> u16 {
        let mut count = 0u16;
        for i in 0..self.item_count() {
            if self.item_id(i).is_some_and(ItemId::is_dead) {
                count += 1;
            }
        }
        count
    }

    /// Returns true if the page can fit a tuple of the given size.
    #[must_use]
    pub fn can_fit(&self, tuple_size: usize) -> bool {
        self.free_space() >= tuple_size + ITEM_ID_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_valid_empty_page() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();
        assert!(page.is_initialized());
        assert_eq!(page.item_count(), 0);
        assert_eq!(page.lower(), PAGE_HEADER_SIZE as u32);
        assert_eq!(page.upper(), PAGE_SIZE as u32);
        assert_eq!(page.free_space(), MAX_USABLE_SPACE);
        assert_eq!(page.page_lsn(), 0);
        assert_eq!(page.flags(), PAGE_FLAG_NONE);
    }

    #[test]
    fn insert_and_read_single_tuple() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();

        let data = b"hello, world!";
        let slot = page.insert_tuple(data).unwrap();
        assert_eq!(slot, 0);
        assert_eq!(page.item_count(), 1);

        let read_back = page.read_tuple(0).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn insert_multiple_tuples() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();

        for i in 0..10 {
            let data = format!("tuple number {i}");
            let slot = page.insert_tuple(data.as_bytes()).unwrap();
            assert_eq!(slot, i as u16);
        }

        assert_eq!(page.item_count(), 10);
        assert_eq!(page.live_tuple_count(), 10);

        for i in 0..10 {
            let expected = format!("tuple number {i}");
            let read_back = page.read_tuple(i as u16).unwrap();
            assert_eq!(read_back, expected.as_bytes());
        }
    }

    #[test]
    fn free_space_decreases_correctly() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();

        let initial_free = page.free_space();
        let data = [0xAB; 100];
        page.insert_tuple(&data).unwrap();

        // Free space should decrease by tuple_size + ITEM_ID_SIZE.
        let expected = initial_free - 100 - ITEM_ID_SIZE;
        assert_eq!(page.free_space(), expected);
    }

    #[test]
    fn can_fit_accounts_for_line_pointer() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();

        let free = page.free_space();
        // A tuple that exactly fills free space minus line pointer should fit.
        assert!(page.can_fit(free - ITEM_ID_SIZE));
        // One byte more should not fit.
        assert!(!page.can_fit(free - ITEM_ID_SIZE + 1));
    }

    #[test]
    fn insert_too_large_tuple_returns_none() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();

        let too_large = vec![0u8; MAX_TUPLE_SIZE + 1];
        assert!(page.insert_tuple(&too_large).is_none());
    }

    #[test]
    fn insert_empty_tuple_returns_none() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();
        assert!(page.insert_tuple(&[]).is_none());
    }

    #[test]
    fn mark_dead_and_compact() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();

        // Insert 5 tuples.
        for i in 0..5 {
            let data = format!("tuple {i}");
            page.insert_tuple(data.as_bytes()).unwrap();
        }

        // Kill tuples 1 and 3.
        assert!(page.mark_dead(1));
        assert!(page.mark_dead(3));
        assert_eq!(page.live_tuple_count(), 3);
        assert_eq!(page.dead_tuple_count(), 2);

        let free_before = page.free_space();
        let removed = page.compact().unwrap();
        assert_eq!(removed, 2);
        assert!(page.free_space() > free_before);

        // After compaction, 3 live tuples remain.
        assert_eq!(page.live_tuple_count(), 3);
        assert_eq!(page.dead_tuple_count(), 0);
        assert_eq!(page.read_tuple(0).unwrap(), b"tuple 0");
        assert!(page.read_tuple(1).is_none());
        assert_eq!(page.item_id(1).unwrap(), ItemId::unused());
        assert_eq!(page.read_tuple(2).unwrap(), b"tuple 2");
        assert!(page.read_tuple(3).is_none());
        assert_eq!(page.read_tuple(4).unwrap(), b"tuple 4");
    }

    #[test]
    fn read_only_view() {
        let mut buf = [0u8; PAGE_SIZE];
        {
            let mut page = HeapPage::from_buf(&mut buf);
            page.init();
            page.insert_tuple(b"test data").unwrap();
        }

        let view = HeapPageRef::from_buf(&buf);
        assert!(view.is_initialized());
        assert_eq!(view.item_count(), 1);
        assert_eq!(view.read_tuple(0).unwrap(), b"test data");
    }

    #[test]
    fn item_id_roundtrip() {
        let id = ItemId::new(4096, 128, ITEM_NORMAL);
        assert_eq!(id.offset(), 4096);
        assert_eq!(id.length(), 128);
        assert_eq!(id.flags(), ITEM_NORMAL);
        assert!(id.is_normal());
        assert!(!id.is_dead());
        assert!(!id.is_unused());

        let bytes = id.to_le_bytes();
        let decoded = ItemId::from_le_bytes(bytes);
        assert_eq!(decoded, id);
    }

    #[test]
    fn page_lsn_roundtrip() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();
        page.set_page_lsn(12345);
        assert_eq!(page.page_lsn(), 12345);
    }

    #[test]
    fn compact_no_dead_tuples_is_noop() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();
        page.insert_tuple(b"live tuple").unwrap();

        let removed = page.compact().unwrap();
        assert_eq!(removed, 0);
        assert_eq!(page.read_tuple(0).unwrap(), b"live tuple");
    }

    #[test]
    fn compact_corrupted_live_tuple_returns_error_and_preserves_page() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();
        page.insert_tuple(b"keep").unwrap();
        page.insert_tuple(b"broken").unwrap();
        assert!(page.mark_dead(0));

        page.set_item_id(1, ItemId::new((PAGE_SIZE - 2) as u16, 8, ITEM_NORMAL));
        let before = *page.data;

        let err = page.compact().unwrap_err();
        assert!(matches!(
            err,
            HeapPageCompactError::CorruptedTuple { slot_index: 1, .. }
        ));
        assert_eq!(page.data, &before);
    }

    #[test]
    fn fill_page_to_capacity() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();

        let tuple_data = [0xABu8; 64];
        let mut count = 0u16;
        while page.insert_tuple(&tuple_data).is_some() {
            count += 1;
        }

        // Should have inserted a reasonable number of 64-byte tuples.
        assert!(count > 100, "expected many tuples, got {count}");
        assert_eq!(page.live_tuple_count(), count);

        // Verify all are readable.
        for i in 0..count {
            assert_eq!(page.read_tuple(i).unwrap(), &tuple_data);
        }
    }

    #[test]
    fn mark_dead_out_of_range_returns_false() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();
        assert!(!page.mark_dead(0));
        assert!(!page.mark_dead(100));
    }

    #[test]
    fn double_mark_dead_returns_false() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();
        page.insert_tuple(b"test").unwrap();
        assert!(page.mark_dead(0));
        assert!(!page.mark_dead(0));
    }

    #[test]
    fn read_tuple_dead_slot_returns_none() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut page = HeapPage::from_buf(&mut buf);
        page.init();
        page.insert_tuple(b"test").unwrap();
        page.mark_dead(0);
        assert!(page.read_tuple(0).is_none());
    }
}
