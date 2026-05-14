//! Page-backed B+tree foundation for persistent ordered indexes.
//!
//! This module is intentionally lower-level than the SQL storage engine. It
//! stores fixed-width `u64 -> u64` entries on 8 KiB pages managed by the shared
//! [`BufferPool`]. The SQL layer can build on this by encoding composite index
//! keys and tuple ids into stable integer/table-space references.
//!
//! Current scope:
//! - persistent metapage with root page and tree height;
//! - leaf pages with sorted entries and right-sibling links;
//! - internal pages with separator keys and child pointers;
//! - recursive insert with page splits and root growth;
//! - point lookup through the buffer pool.
//!
//! # On-disk format
//!
//! All pages share a 32-byte header:
//!
//! ```text
//! offset  size  field
//! ------  ----  ------------------------------------
//!   0      8    magic ("AIONBTM1" / "AIONBTB1")
//!   8      2    page kind (1=leaf, 2=internal)
//!  10      6    item count + reserved (little-endian, low 2 bytes are count)
//!  16      8    right-sibling page number (NO_PAGE on rightmost leaf)
//!  24      8    first-child page number (internal only) / reserved (leaf)
//! ```
//!
//! Leaf pages append `(key:u64, tuple_id:u64)` entries after the header,
//! 16 bytes each, sorted ascending by `(key, tuple_id)`. Internal pages
//! follow the first-child slot with `(separator_key:u64, child_page:u64)`
//! pairs.
//!
//! The metapage uses the same magic prefix (`META_MAGIC`) and stores
//! `root_page`, `height`, and `page_count`.
//!
//! # Out of scope for this slice
//!
//! - delete/rebalance: general sibling redistribution is not implemented yet.
//!   Safe local compaction does exist: empty leaves can be unlinked from the
//!   global leaf chain, removed from their parent child slot, recycled through
//!   a simple free list, trivial one-child ancestors can collapse toward the
//!   root, and a leaf can merge with its left or right sibling when both
//!   payloads fit on one page and the sibling relation is locally safe to
//!   update. When merge would not fit, sibling-local redistribution can also
//!   rebalance two adjacent leaves under the same parent. The delete path also
//!   performs a first internal-page counterpart for sparse deep trees:
//!   sibling-local redistribution and merge between adjacent internal pages
//!   under the same parent, plus ancestor-separator refresh when a subtree
//!   minimum changes, including after leftmost internal collapse. When the
//!   root is an internal node with two leaf children whose combined payload
//!   fits on one page, delete can also collapse that pair back into a single
//!   leaf root; the same idea now also exists one level up for a two-child
//!   internal root whose internal children can be merged into one page. These
//!   local maintenance steps are now gated by explicit minimum-occupancy
//!   thresholds derived from page capacity rather than ad-hoc shape checks
//!   alone. Other sparse pages still rely on a future VACUUM-like pass or
//!   broader redistribution logic for fuller compaction.
//! - true variable-length SQL key encoding (full TEXT/UUID/composite ranges).
//! - page-level WAL redo records: index pages are still rebuilt from
//!   table rows on recovery (see `disk_ordered_index` in storage-engine).
//!
//! # Concurrency
//!
//! A single [`DiskBTree`] handle is `Send + Sync`-safe under the buffer
//! pool's per-frame mutexes; mutations are serialised via the page
//! `RwLockWriteGuard`. Long traversals do **not** hold a read guard on
//! ancestors while descending, so a concurrent insert that splits a leaf
//! cannot stall a reader. Range scans follow leaf right-sibling links
//! without taking any extra locks.

#![allow(
    clippy::map_unwrap_or,
    clippy::missing_errors_doc,
    clippy::unnecessary_wraps
)]

use std::sync::Arc;

use crate::page::{PageId, PAGE_SIZE};
use crate::pool::{BufferPool, BufferPoolError, BufferPoolResult};

#[path = "disk_btree_maintenance.rs"]
mod disk_btree_maintenance;

const META_MAGIC: &[u8; 8] = b"AIONBTM1";
const PAGE_MAGIC: &[u8; 8] = b"AIONBTB1";
const META_PAGE_NO: u64 = 0;
const PAGE_HEADER_SIZE: usize = 32;
const META_ROOT_OFFSET: usize = 8;
const META_HEIGHT_OFFSET: usize = 16;
const META_PAGE_COUNT_OFFSET: usize = 20;
const META_FREE_LIST_OFFSET: usize = 28;
const PAGE_KIND_OFFSET: usize = 8;
const PAGE_COUNT_OFFSET: usize = 10;
const PAGE_RIGHT_SIBLING_OFFSET: usize = 16;
const PAGE_FIRST_CHILD_OFFSET: usize = 24;
const LEAF_ENTRY_SIZE: usize = 16;
const INTERNAL_ENTRY_SIZE: usize = 16;
const NO_PAGE: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PageKind {
    Leaf = 1,
    Internal = 2,
}

impl PageKind {
    fn from_byte(raw: u8) -> BufferPoolResult<Self> {
        match raw {
            1 => Ok(Self::Leaf),
            2 => Ok(Self::Internal),
            _ => Err(corrupt(format!("invalid btree page kind {raw}"))),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiskBTreeConfig {
    pub relation_id: u64,
    pub leaf_capacity: usize,
    pub internal_capacity: usize,
}

impl DiskBTreeConfig {
    #[must_use]
    pub fn new(relation_id: u64) -> Self {
        Self {
            relation_id,
            leaf_capacity: (PAGE_SIZE - PAGE_HEADER_SIZE) / LEAF_ENTRY_SIZE,
            internal_capacity: (PAGE_SIZE - PAGE_HEADER_SIZE - 8) / INTERNAL_ENTRY_SIZE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskBTreeStats {
    pub root_page: u64,
    pub height: u32,
    pub page_count: u64,
    pub leaf_capacity: usize,
    pub internal_capacity: usize,
}

pub struct DiskBTree {
    pool: Arc<BufferPool>,
    config: DiskBTreeConfig,
}

impl std::fmt::Debug for DiskBTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskBTree")
            .field("relation_id", &self.config.relation_id)
            .field("leaf_capacity", &self.config.leaf_capacity)
            .field("internal_capacity", &self.config.internal_capacity)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Split {
    separator: u64,
    right_page: u64,
}

#[derive(Clone, Copy, Debug)]
struct PathFrame {
    page_no: u64,
    child_slot: usize,
}

impl DiskBTree {
    #[inline]
    fn min_leaf_entries(&self) -> usize {
        self.config.leaf_capacity.max(2).div_ceil(2)
    }

    #[inline]
    fn min_internal_entries(&self) -> usize {
        self.config
            .internal_capacity
            .max(2)
            .div_ceil(2)
            .saturating_sub(1)
    }

    /// Open an existing tree or create a new one for `relation_id`.
    ///
    /// Page 0 is the metapage. A new tree allocates page 0 and page 1, making
    /// page 1 the first leaf/root page.
    pub fn open_or_create(
        pool: Arc<BufferPool>,
        config: DiskBTreeConfig,
    ) -> BufferPoolResult<Self> {
        let tree = Self { pool, config };
        let mut initialize = false;
        match tree.fetch_page(META_PAGE_NO) {
            Ok(guard) => {
                let page = guard.read();
                if page.data()[..META_MAGIC.len()] != *META_MAGIC {
                    if page.data().iter().all(|byte| *byte == 0) {
                        initialize = true;
                    } else {
                        return Err(corrupt("btree metapage has invalid magic"));
                    }
                }
            }
            Err(BufferPoolError::Io(ref err)) if err.kind() == std::io::ErrorKind::NotFound => {
                initialize = true;
            }
            Err(err) => return Err(err),
        }
        if initialize {
            tree.pool.reset_relation(tree.config.relation_id)?;
            tree.initialize_empty()?;
        }
        Ok(tree)
    }

    #[must_use]
    pub fn relation_id(&self) -> u64 {
        self.config.relation_id
    }

    pub fn stats(&self) -> BufferPoolResult<DiskBTreeStats> {
        Ok(DiskBTreeStats {
            root_page: self.root_page()?,
            height: self.height()?,
            page_count: self.page_count()?,
            leaf_capacity: self.config.leaf_capacity,
            internal_capacity: self.config.internal_capacity,
        })
    }

    pub fn get(&self, key: u64) -> BufferPoolResult<Option<u64>> {
        let mut page_no = self.root_page()?;
        loop {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            match read_page_kind(page.data())? {
                PageKind::Leaf => return leaf_get(page.data(), key),
                PageKind::Internal => {
                    page_no = internal_child_for_key(page.data(), key)?;
                }
            }
        }
    }

    /// Return entries whose keys are in `[lower, upper]`, ordered by key.
    ///
    /// `None` bounds are unbounded. The implementation descends to the first
    /// candidate leaf and then follows leaf right-sibling links, matching the
    /// access pattern expected from a B+tree range scan.
    pub fn range(
        &self,
        lower: Option<u64>,
        upper: Option<u64>,
        limit: Option<usize>,
    ) -> BufferPoolResult<Vec<(u64, u64)>> {
        if limit == Some(0) {
            return Ok(Vec::new());
        }

        let mut page_no = match lower {
            Some(key) => self.leaf_for_key(key)?,
            None => self.leftmost_leaf()?,
        };
        let mut out = Vec::new();

        loop {
            let (entries, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_leaf_entries(page.data())?,
                    read_right_sibling(page.data()),
                )
            };

            for (key, value) in entries {
                if lower.is_some_and(|lower| key < lower) {
                    continue;
                }
                if upper.is_some_and(|upper| key > upper) {
                    return Ok(out);
                }
                out.push((key, value));
                if limit.is_some_and(|limit| out.len() >= limit) {
                    return Ok(out);
                }
            }

            if right_sibling == NO_PAGE {
                return Ok(out);
            }
            page_no = right_sibling;
        }
    }

    /// Return entries whose keys are in `[lower, upper]`, ordered by key
    /// descending. The on-disk leaves are singly linked to the right, so this
    /// first collects the relevant leaf page numbers, then walks them in
    /// reverse. It still avoids materializing row payloads for Top-N scans.
    pub fn range_desc(
        &self,
        lower: Option<u64>,
        upper: Option<u64>,
        limit: Option<usize>,
    ) -> BufferPoolResult<Vec<(u64, u64)>> {
        if limit == Some(0) {
            return Ok(Vec::new());
        }

        if lower.is_none() && upper.is_none() {
            if let Some(limit) = limit {
                let page_no = self.rightmost_leaf()?;
                let entries = {
                    let guard = self.fetch_page(page_no)?;
                    let page = guard.read();
                    read_leaf_entries(page.data())?
                };
                if entries.len() >= limit {
                    return Ok(entries.into_iter().rev().take(limit).collect());
                }
            }
        }

        let mut page_no = match lower {
            Some(key) => self.leaf_for_key(key)?,
            None => self.leftmost_leaf()?,
        };
        let mut leaf_pages = Vec::new();
        loop {
            let (entries, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_leaf_entries(page.data())?,
                    read_right_sibling(page.data()),
                )
            };
            if entries
                .last()
                .is_some_and(|(key, _)| upper.map_or(true, |upper| *key <= upper))
                || entries
                    .first()
                    .is_some_and(|(key, _)| upper.map_or(true, |upper| *key <= upper))
            {
                leaf_pages.push(page_no);
            }
            if right_sibling == NO_PAGE {
                break;
            }
            page_no = right_sibling;
        }

        let mut out = Vec::new();
        for page_no in leaf_pages.into_iter().rev() {
            let entries = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                read_leaf_entries(page.data())?
            };
            for (key, value) in entries.into_iter().rev() {
                if upper.is_some_and(|upper| key > upper) {
                    continue;
                }
                if lower.is_some_and(|lower| key < lower) {
                    return Ok(out);
                }
                out.push((key, value));
                if limit.is_some_and(|limit| out.len() >= limit) {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    pub fn insert(&self, key: u64, value: u64) -> BufferPoolResult<()> {
        let root = self.root_page()?;
        if let Some(split) = self.insert_recursive(root, key, value)? {
            let new_root = self.allocate_initialized_page(PageKind::Internal)?;
            {
                let guard = self.fetch_page(new_root)?;
                let mut page = guard.write();
                let data = page.data_mut();
                write_count(data, 1)?;
                write_first_child(data, root);
                write_internal_entry(data, 0, split.separator, split.right_page)?;
            }
            self.set_root(new_root, self.height()?.saturating_add(1))?;
        }
        Ok(())
    }

    /// Rebuild the tree from sorted `(key, value)` entries.
    ///
    /// This is intended for recovery/checkpoint index rebuilds where the SQL
    /// layer already scans all rows. It avoids O(n log n) point inserts and
    /// writes leaf/internal pages sequentially.
    pub fn bulk_load_sorted(&self, entries: &[(u64, u64)]) -> BufferPoolResult<()> {
        self.pool.reset_relation(self.config.relation_id)?;
        self.initialize_meta_page()?;
        if entries.is_empty() {
            let root_no = self.allocate_initialized_page(PageKind::Leaf)?;
            self.set_root(root_no, 1)?;
            self.set_page_count(root_no.saturating_add(1))?;
            return Ok(());
        }

        let mut level = Vec::new();
        for chunk in entries.chunks(self.config.leaf_capacity.max(1)) {
            let page_no = self.allocate_initialized_page(PageKind::Leaf)?;
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            write_leaf_entries(page.data_mut(), chunk)?;
            level.push((chunk[0].0, page_no));
        }
        for window in level.windows(2) {
            let (_, page_no) = window[0];
            let (_, right_page_no) = window[1];
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            write_right_sibling(page.data_mut(), right_page_no);
        }

        let mut height = 1_u32;
        while level.len() > 1 {
            height = height.saturating_add(1);
            let mut next_level = Vec::new();
            for group in level.chunks(self.config.internal_capacity.saturating_add(1).max(2)) {
                let page_no = self.allocate_initialized_page(PageKind::Internal)?;
                let first_child = group[0].1;
                let entries = group[1..].to_vec();
                let guard = self.fetch_page(page_no)?;
                let mut page = guard.write();
                write_internal_entries(page.data_mut(), first_child, &entries)?;
                next_level.push((group[0].0, page_no));
            }
            level = next_level;
        }

        self.set_root(level[0].1, height)?;
        self.flush()
    }

    /// Delete a key from the tree.
    ///
    /// This slice still does not implement full sibling redistribution or
    /// general merge/rebalance. It does perform safe local compaction when a
    /// leaf becomes empty: the leaf is unlinked from the sibling chain, the
    /// parent child slot is removed, and trivial one-child internal pages can
    /// collapse upward toward the root.
    pub fn delete(&self, key: u64) -> BufferPoolResult<bool> {
        let (leaf, path) = self.leaf_for_key_with_path(key)?;
        let mut entries = {
            let guard = self.fetch_page(leaf)?;
            let page = guard.read();
            read_leaf_entries(page.data())?
        };
        let old_first_key = entries.first().map(|(entry_key, _)| *entry_key);
        let Ok(idx) = entries.binary_search_by_key(&key, |(entry_key, _)| *entry_key) else {
            return Ok(false);
        };
        entries.remove(idx);
        let guard = self.fetch_page(leaf)?;
        let mut page = guard.write();
        write_leaf_entries(page.data_mut(), &entries)?;
        let right_sibling = read_right_sibling(page.data());
        drop(page);
        drop(guard);
        if entries.is_empty() {
            self.compact_empty_leaf(leaf, right_sibling, &path)?;
        } else {
            let new_first_key = entries.first().map(|(entry_key, _)| *entry_key);
            if new_first_key != old_first_key {
                if let Some(new_first_key) = new_first_key {
                    self.refresh_ancestor_separators_after_child_min_change(&path, new_first_key)?;
                }
            }
            if entries.len() < self.min_leaf_entries()
                && !self.try_merge_leaf_with_right_sibling(leaf, &entries, right_sibling, &path)?
                && !self.try_merge_leaf_with_left_sibling(leaf, &path)?
                && !self.try_redistribute_leaf_with_right_sibling(
                    leaf,
                    &entries,
                    right_sibling,
                    &path,
                )?
            {
                self.try_redistribute_leaf_with_left_sibling(leaf, &path)?;
            }
        }
        self.collapse_root_pairs_if_possible()?;
        Ok(true)
    }

    pub fn flush(&self) -> BufferPoolResult<()> {
        self.pool.flush_all()?;
        self.pool.sync()
    }

    fn initialize_empty(&self) -> BufferPoolResult<()> {
        self.initialize_meta_page()?;

        let root_no = self.allocate_initialized_page(PageKind::Leaf)?;
        self.set_root(root_no, 1)?;
        self.set_page_count(root_no.saturating_add(1))?;
        Ok(())
    }

    fn initialize_meta_page(&self) -> BufferPoolResult<()> {
        let meta = self.pool.new_page(self.config.relation_id)?;
        if meta.page_id().page_number != META_PAGE_NO {
            return Err(corrupt("btree metapage allocation did not return page 0"));
        }
        {
            let mut page = meta.write();
            let data = page.data_mut();
            data.fill(0);
            data[..META_MAGIC.len()].copy_from_slice(META_MAGIC);
            write_u64(data, META_FREE_LIST_OFFSET, NO_PAGE)?;
        }
        drop(meta);
        Ok(())
    }

    fn insert_recursive(
        &self,
        page_no: u64,
        key: u64,
        value: u64,
    ) -> BufferPoolResult<Option<Split>> {
        let kind = {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            read_page_kind(page.data())?
        };

        match kind {
            PageKind::Leaf => self.insert_leaf(page_no, key, value),
            PageKind::Internal => {
                let child = {
                    let guard = self.fetch_page(page_no)?;
                    let page = guard.read();
                    internal_child_for_key(page.data(), key)?
                };
                let Some(child_split) = self.insert_recursive(child, key, value)? else {
                    return Ok(None);
                };
                self.insert_internal_entry(page_no, child_split)
            }
        }
    }

    fn insert_leaf(&self, page_no: u64, key: u64, value: u64) -> BufferPoolResult<Option<Split>> {
        let mut entries = {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            read_leaf_entries(page.data())?
        };

        match entries.binary_search_by_key(&key, |(entry_key, _)| *entry_key) {
            Ok(idx) => entries[idx].1 = value,
            Err(idx) => entries.insert(idx, (key, value)),
        }

        if entries.len() <= self.config.leaf_capacity {
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            write_leaf_entries(page.data_mut(), &entries)?;
            return Ok(None);
        }

        let right_page = self.allocate_initialized_page(PageKind::Leaf)?;
        let split_at = entries.len() / 2;
        let right_entries = entries.split_off(split_at);
        let separator = right_entries[0].0;
        let old_right = {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            read_right_sibling(page.data())
        };

        {
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &entries)?;
            write_right_sibling(data, right_page);
        }
        {
            let guard = self.fetch_page(right_page)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &right_entries)?;
            write_right_sibling(data, old_right);
        }

        Ok(Some(Split {
            separator,
            right_page,
        }))
    }

    fn insert_internal_entry(&self, page_no: u64, split: Split) -> BufferPoolResult<Option<Split>> {
        let first_child;
        let mut entries;
        {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            first_child = read_first_child(page.data());
            entries = read_internal_entries(page.data())?;
        }

        let idx = entries.partition_point(|(separator, _)| *separator <= split.separator);
        entries.insert(idx, (split.separator, split.right_page));

        if entries.len() <= self.config.internal_capacity {
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), first_child, &entries)?;
            return Ok(None);
        }

        let split_at = entries.len() / 2;
        let promoted = entries[split_at].0;
        let right_first_child = entries[split_at].1;
        let right_entries = entries.split_off(split_at + 1);

        let right_page = self.allocate_initialized_page(PageKind::Internal)?;
        {
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), first_child, &entries[..split_at])?;
        }
        {
            let guard = self.fetch_page(right_page)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), right_first_child, &right_entries)?;
        }

        Ok(Some(Split {
            separator: promoted,
            right_page,
        }))
    }

    fn allocate_initialized_page(&self, kind: PageKind) -> BufferPoolResult<u64> {
        let page_no = if let Some(page_no) = self.pop_free_page()? {
            page_no
        } else {
            let guard = self.pool.new_page(self.config.relation_id)?;
            let page_no = guard.page_id().page_number;
            let next_page_count = page_no
                .saturating_add(1)
                .max(self.page_count().unwrap_or(0));
            self.set_page_count(next_page_count)?;
            page_no
        };
        {
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            let data = page.data_mut();
            data.fill(0);
            data[..PAGE_MAGIC.len()].copy_from_slice(PAGE_MAGIC);
            data[PAGE_KIND_OFFSET] = kind as u8;
            write_count(data, 0)?;
            write_right_sibling(data, NO_PAGE);
            write_first_child(data, NO_PAGE);
        }
        Ok(page_no)
    }

    fn leaf_for_key(&self, key: u64) -> BufferPoolResult<u64> {
        self.leaf_for_key_with_path(key).map(|(leaf, _)| leaf)
    }

    fn leaf_for_key_with_path(&self, key: u64) -> BufferPoolResult<(u64, Vec<PathFrame>)> {
        let mut page_no = self.root_page()?;
        let mut path = Vec::new();
        loop {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            match read_page_kind(page.data())? {
                PageKind::Leaf => return Ok((page_no, path)),
                PageKind::Internal => {
                    let (child, child_slot) = internal_child_for_key_with_slot(page.data(), key)?;
                    path.push(PathFrame {
                        page_no,
                        child_slot,
                    });
                    page_no = child;
                }
            }
        }
    }

    fn leftmost_leaf(&self) -> BufferPoolResult<u64> {
        let mut page_no = self.root_page()?;
        loop {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            match read_page_kind(page.data())? {
                PageKind::Leaf => return Ok(page_no),
                PageKind::Internal => {
                    page_no = read_first_child(page.data());
                    if page_no == NO_PAGE {
                        return Err(corrupt("btree internal page has no first child"));
                    }
                }
            }
        }
    }

    fn rightmost_leaf(&self) -> BufferPoolResult<u64> {
        let mut page_no = self.root_page()?;
        loop {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            match read_page_kind(page.data())? {
                PageKind::Leaf => return Ok(page_no),
                PageKind::Internal => {
                    let entries = read_internal_entries(page.data())?;
                    page_no = entries
                        .last()
                        .map_or_else(|| read_first_child(page.data()), |(_, child)| *child);
                    if page_no == NO_PAGE {
                        return Err(corrupt("btree internal page has no rightmost child"));
                    }
                }
            }
        }
    }

    fn fetch_page(&self, page_number: u64) -> BufferPoolResult<crate::pool::PageGuard<'_>> {
        self.pool.fetch_page(PageId {
            relation_id: self.config.relation_id,
            page_number,
        })
    }

    fn root_page(&self) -> BufferPoolResult<u64> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let page = guard.read();
        read_u64(page.data(), META_ROOT_OFFSET)
    }

    fn height(&self) -> BufferPoolResult<u32> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let page = guard.read();
        read_u32(page.data(), META_HEIGHT_OFFSET)
    }

    fn page_count(&self) -> BufferPoolResult<u64> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let page = guard.read();
        read_u64(page.data(), META_PAGE_COUNT_OFFSET)
    }

    fn free_list_head(&self) -> BufferPoolResult<u64> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let page = guard.read();
        let page_no = read_u64(page.data(), META_FREE_LIST_OFFSET)?;
        if page_no == META_PAGE_NO {
            Ok(NO_PAGE)
        } else {
            Ok(page_no)
        }
    }

    fn set_free_list_head(&self, page_no: u64) -> BufferPoolResult<()> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let mut page = guard.write();
        write_u64(page.data_mut(), META_FREE_LIST_OFFSET, page_no)
    }

    fn pop_free_page(&self) -> BufferPoolResult<Option<u64>> {
        let page_no = self.free_list_head()?;
        if page_no == NO_PAGE {
            return Ok(None);
        }
        let next = {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            read_right_sibling(page.data())
        };
        self.set_free_list_head(next)?;
        Ok(Some(page_no))
    }

    fn push_free_page(&self, page_no: u64) -> BufferPoolResult<()> {
        let old_head = self.free_list_head()?;
        let guard = self.fetch_page(page_no)?;
        let mut page = guard.write();
        let data = page.data_mut();
        data.fill(0);
        data[..PAGE_MAGIC.len()].copy_from_slice(PAGE_MAGIC);
        data[PAGE_KIND_OFFSET] = PageKind::Leaf as u8;
        write_count(data, 0)?;
        write_right_sibling(data, old_head);
        write_first_child(data, NO_PAGE);
        drop(page);
        drop(guard);
        self.set_free_list_head(page_no)
    }

    fn set_root(&self, root_page: u64, height: u32) -> BufferPoolResult<()> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let mut page = guard.write();
        let data = page.data_mut();
        write_u64(data, META_ROOT_OFFSET, root_page)?;
        write_u32(data, META_HEIGHT_OFFSET, height)?;
        Ok(())
    }

    fn set_page_count(&self, page_count: u64) -> BufferPoolResult<()> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let mut page = guard.write();
        write_u64(page.data_mut(), META_PAGE_COUNT_OFFSET, page_count)
    }
}

fn corrupt(message: impl Into<String>) -> BufferPoolError {
    BufferPoolError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

fn read_page_kind(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<PageKind> {
    if data[..PAGE_MAGIC.len()] != *PAGE_MAGIC {
        return Err(corrupt("btree page has invalid magic"));
    }
    PageKind::from_byte(data[PAGE_KIND_OFFSET])
}

fn read_count(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<usize> {
    let raw = u16::from_le_bytes([data[PAGE_COUNT_OFFSET], data[PAGE_COUNT_OFFSET + 1]]);
    Ok(usize::from(raw))
}

fn write_count(data: &mut [u8; PAGE_SIZE], count: usize) -> BufferPoolResult<()> {
    let raw = u16::try_from(count).map_err(|_| corrupt("btree page entry count exceeds u16"))?;
    data[PAGE_COUNT_OFFSET..PAGE_COUNT_OFFSET + 2].copy_from_slice(&raw.to_le_bytes());
    Ok(())
}

fn read_right_sibling(data: &[u8; PAGE_SIZE]) -> u64 {
    read_u64(data, PAGE_RIGHT_SIBLING_OFFSET).unwrap_or(NO_PAGE)
}

fn write_right_sibling(data: &mut [u8; PAGE_SIZE], page_no: u64) {
    let _ = write_u64(data, PAGE_RIGHT_SIBLING_OFFSET, page_no);
}

fn read_first_child(data: &[u8; PAGE_SIZE]) -> u64 {
    read_u64(data, PAGE_FIRST_CHILD_OFFSET).unwrap_or(NO_PAGE)
}

fn write_first_child(data: &mut [u8; PAGE_SIZE], page_no: u64) {
    let _ = write_u64(data, PAGE_FIRST_CHILD_OFFSET, page_no);
}

fn leaf_get(data: &[u8; PAGE_SIZE], key: u64) -> BufferPoolResult<Option<u64>> {
    let entries = read_leaf_entries(data)?;
    Ok(entries
        .binary_search_by_key(&key, |(entry_key, _)| *entry_key)
        .ok()
        .map(|idx| entries[idx].1))
}

fn read_leaf_entries(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<Vec<(u64, u64)>> {
    if read_page_kind(data)? != PageKind::Leaf {
        return Err(corrupt("expected btree leaf page"));
    }
    let count = read_count(data)?;
    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = PAGE_HEADER_SIZE + idx * LEAF_ENTRY_SIZE;
        entries.push((read_u64(data, offset)?, read_u64(data, offset + 8)?));
    }
    Ok(entries)
}

fn write_leaf_entries(data: &mut [u8; PAGE_SIZE], entries: &[(u64, u64)]) -> BufferPoolResult<()> {
    data[PAGE_HEADER_SIZE..].fill(0);
    write_count(data, entries.len())?;
    for (idx, (key, value)) in entries.iter().copied().enumerate() {
        let offset = PAGE_HEADER_SIZE + idx * LEAF_ENTRY_SIZE;
        write_u64(data, offset, key)?;
        write_u64(data, offset + 8, value)?;
    }
    Ok(())
}

fn internal_child_for_key(data: &[u8; PAGE_SIZE], key: u64) -> BufferPoolResult<u64> {
    internal_child_for_key_with_slot(data, key).map(|(child, _)| child)
}

fn internal_child_for_key_with_slot(
    data: &[u8; PAGE_SIZE],
    key: u64,
) -> BufferPoolResult<(u64, usize)> {
    if read_page_kind(data)? != PageKind::Internal {
        return Err(corrupt("expected btree internal page"));
    }
    let mut child = read_first_child(data);
    let entries = read_internal_entries(data)?;
    for (idx, (separator, right_child)) in entries.iter().copied().enumerate() {
        if key < separator {
            return Ok((child, idx));
        }
        child = right_child;
    }
    Ok((child, entries.len()))
}

fn read_internal_entries(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<Vec<(u64, u64)>> {
    if read_page_kind(data)? != PageKind::Internal {
        return Err(corrupt("expected btree internal page"));
    }
    let count = read_count(data)?;
    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = PAGE_HEADER_SIZE + idx * INTERNAL_ENTRY_SIZE;
        entries.push((read_u64(data, offset)?, read_u64(data, offset + 8)?));
    }
    Ok(entries)
}

fn write_internal_entries(
    data: &mut [u8; PAGE_SIZE],
    first_child: u64,
    entries: &[(u64, u64)],
) -> BufferPoolResult<()> {
    data[PAGE_HEADER_SIZE..].fill(0);
    write_first_child(data, first_child);
    write_count(data, entries.len())?;
    for (idx, (separator, child)) in entries.iter().copied().enumerate() {
        write_internal_entry(data, idx, separator, child)?;
    }
    Ok(())
}

fn write_internal_entry(
    data: &mut [u8; PAGE_SIZE],
    idx: usize,
    separator: u64,
    child: u64,
) -> BufferPoolResult<()> {
    let offset = PAGE_HEADER_SIZE + idx * INTERNAL_ENTRY_SIZE;
    write_u64(data, offset, separator)?;
    write_u64(data, offset + 8, child)
}

fn read_u32(data: &[u8; PAGE_SIZE], offset: usize) -> BufferPoolResult<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| corrupt("btree read offset overflow"))?;
    let bytes = data
        .get(offset..end)
        .ok_or_else(|| corrupt("btree read past page boundary"))?;
    let mut out = [0u8; 4];
    out.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(out))
}

fn write_u32(data: &mut [u8; PAGE_SIZE], offset: usize, value: u32) -> BufferPoolResult<()> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| corrupt("btree write offset overflow"))?;
    let slot = data
        .get_mut(offset..end)
        .ok_or_else(|| corrupt("btree write past page boundary"))?;
    slot.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_u64(data: &[u8; PAGE_SIZE], offset: usize) -> BufferPoolResult<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| corrupt("btree read offset overflow"))?;
    let bytes = data
        .get(offset..end)
        .ok_or_else(|| corrupt("btree read past page boundary"))?;
    let mut out = [0u8; 8];
    out.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(out))
}

fn write_u64(data: &mut [u8; PAGE_SIZE], offset: usize, value: u64) -> BufferPoolResult<()> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| corrupt("btree write offset overflow"))?;
    let slot = data
        .get_mut(offset..end)
        .ok_or_else(|| corrupt("btree write past page boundary"))?;
    slot.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
#[path = "disk_btree_tests.rs"]
mod tests;
