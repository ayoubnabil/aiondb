//! Page-backed variable-length ordered index prototype.
//!
//! First storage primitive toward PostgreSQL-style variable-length index
//! keys. Starts with a sorted leaf chain and bulk-load support rather than
//! a mutable multi-level tree, and now also supports online leaf-chain
//! inserts, deletes and leaf splits. Gives the storage layer a durable
//! page format for full-width `TEXT`/`UUID`/composite keys before fully
//! incremental internal-page splits, leaf merge/rebalance and page-level
//! WAL redo land.

#![allow(clippy::missing_errors_doc)]

use std::sync::Arc;

use crate::page::{PageId, PAGE_SIZE};
use crate::pool::{BufferPool, BufferPoolError, BufferPoolResult};

const META_MAGIC: &[u8; 8] = b"AIONVTM1";
const LEAF_MAGIC: &[u8; 8] = b"AIONVTL1";
const META_PAGE_NO: u64 = 0;
const META_FIRST_LEAF_OFFSET: usize = 8;
const META_PAGE_COUNT_OFFSET: usize = 16;
const META_FREE_LIST_OFFSET: usize = 24;
const META_FIRST_INTERNAL_OFFSET: usize = 32;
const META_INTERNAL_LEVELS_OFFSET: usize = 40;
const LEAF_COUNT_OFFSET: usize = 8;
const LEAF_RIGHT_SIBLING_OFFSET: usize = 16;
const INTERNAL_MAGIC: &[u8; 8] = b"AIONVTI1";
const INTERNAL_COUNT_OFFSET: usize = 8;
const INTERNAL_RIGHT_SIBLING_OFFSET: usize = 16;
const INTERNAL_HEADER_SIZE: usize = 24;
const LEAF_HEADER_SIZE: usize = 24;
const ENTRY_HEADER_SIZE: usize = 10;
const NO_PAGE: u64 = u64::MAX;

#[derive(Clone, Debug)]
pub struct DiskVarBTreeConfig {
    pub relation_id: u64,
}

impl DiskVarBTreeConfig {
    #[must_use]
    pub fn new(relation_id: u64) -> Self {
        Self { relation_id }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VarEntry {
    pub key: Vec<u8>,
    pub value: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiskVarBTreeStats {
    pub allocated_pages: u64,
    pub linked_leaf_pages: u64,
    pub empty_leaf_pages: u64,
    pub free_leaf_pages: u64,
    pub internal_pages: u64,
    pub live_entries: u64,
    pub payload_bytes: u64,
    pub max_leaf_entries: u16,
}

pub struct DiskVarBTree {
    pool: Arc<BufferPool>,
    config: DiskVarBTreeConfig,
}

impl DiskVarBTree {
    pub fn open_or_create(
        pool: Arc<BufferPool>,
        config: DiskVarBTreeConfig,
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
                        return Err(corrupt("var-btree metapage has invalid magic"));
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

    pub fn bulk_load_sorted(&self, entries: &[VarEntry]) -> BufferPoolResult<()> {
        validate_sorted_entries(entries)?;
        self.pool.reset_relation(self.config.relation_id)?;
        self.initialize_meta_page()?;

        let mut page_entries = Vec::new();
        let mut current_size = LEAF_HEADER_SIZE;
        for entry in entries {
            let entry_size = ENTRY_HEADER_SIZE
                .checked_add(entry.key.len())
                .ok_or_else(|| corrupt("var-btree entry size overflow"))?;
            if entry_size > PAGE_SIZE - LEAF_HEADER_SIZE {
                return Err(corrupt("var-btree key does not fit on one leaf page"));
            }
            if !page_entries.is_empty() && current_size + entry_size > PAGE_SIZE {
                self.append_leaf_page(&page_entries)?;
                page_entries.clear();
                current_size = LEAF_HEADER_SIZE;
            }
            page_entries.push(entry.clone());
            current_size += entry_size;
        }
        self.append_leaf_page(&page_entries)?;
        self.link_leaf_pages()?;
        self.rebuild_leaf_directory()?;
        self.flush()
    }

    pub fn range(
        &self,
        lower: Option<&[u8]>,
        upper: Option<&[u8]>,
        limit: Option<usize>,
    ) -> BufferPoolResult<Vec<VarEntry>> {
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let mut page_no = match lower {
            Some(lower) => self.leaf_for_lower_bound(lower)?,
            None => self.first_leaf()?,
        };
        let mut out = Vec::new();
        while page_no != NO_PAGE {
            let (entries, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_leaf_entries(page.data())?,
                    read_right_sibling(page.data()),
                )
            };
            for entry in entries {
                if lower.is_some_and(|lower| entry.key.as_slice() < lower) {
                    continue;
                }
                if upper.is_some_and(|upper| entry.key.as_slice() > upper) {
                    return Ok(out);
                }
                out.push(entry);
                if limit.is_some_and(|limit| out.len() >= limit) {
                    return Ok(out);
                }
            }
            page_no = right_sibling;
        }
        Ok(out)
    }

    pub fn get_values(&self, key: &[u8]) -> BufferPoolResult<Vec<u64>> {
        let mut page_no = self.leaf_for_lower_bound(key)?;
        let mut out = Vec::new();
        while page_no != NO_PAGE {
            let (entries, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_leaf_entries(page.data())?,
                    read_right_sibling(page.data()),
                )
            };
            for entry in entries {
                match entry.key.as_slice().cmp(key) {
                    std::cmp::Ordering::Less => {}
                    std::cmp::Ordering::Equal => out.push(entry.value),
                    std::cmp::Ordering::Greater => return Ok(out),
                }
            }
            page_no = right_sibling;
        }
        Ok(out)
    }

    pub fn insert(&self, key: Vec<u8>, value: u64) -> BufferPoolResult<()> {
        let entry_size = entry_size_for_key(&key)?;
        if entry_size > PAGE_SIZE - LEAF_HEADER_SIZE {
            return Err(corrupt("var-btree key does not fit on one leaf page"));
        }
        let had_directory = self.first_internal()? != NO_PAGE;
        let entry = VarEntry { key, value };
        let mut page_no = self.leaf_for_insert(&entry.key)?;
        if page_no == NO_PAGE {
            page_no = self.allocate_leaf_page()?;
            self.set_first_leaf(page_no)?;
        }

        loop {
            let (mut entries, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_leaf_entries(page.data())?,
                    read_right_sibling(page.data()),
                )
            };
            let should_insert_here = right_sibling == NO_PAGE
                || entries
                    .last()
                    .map_or(true, |last| entry.key.as_slice() <= last.key.as_slice());
            if !should_insert_here {
                page_no = right_sibling;
                continue;
            }

            let idx = entries.partition_point(|existing| {
                (existing.key.as_slice(), existing.value) < (entry.key.as_slice(), entry.value)
            });
            if entries
                .get(idx)
                .is_some_and(|existing| existing.key == entry.key && existing.value == entry.value)
            {
                return Ok(());
            }
            let old_first_key = entries.first().map(|entry| entry.key.clone());
            entries.insert(idx, entry);
            if leaf_payload_size(&entries)? <= PAGE_SIZE {
                let first_key_changed =
                    old_first_key != entries.first().map(|entry| entry.key.clone());
                let guard = self.fetch_page(page_no)?;
                let mut page = guard.write();
                let data = page.data_mut();
                write_leaf_entries(data, &entries)?;
                write_right_sibling(data, right_sibling)?;
                drop(page);
                drop(guard);
                self.refresh_leaf_directory_after_mutation(had_directory, first_key_changed)?;
                return Ok(());
            }
            self.split_leaf(page_no, entries, right_sibling)?;
            self.refresh_leaf_directory_after_mutation(had_directory, true)?;
            return Ok(());
        }
    }

    pub fn delete(&self, key: &[u8], value: u64) -> BufferPoolResult<bool> {
        let had_directory = self.first_internal()? != NO_PAGE;
        let mut page_no = self.leaf_for_lower_bound(key)?;
        while page_no != NO_PAGE {
            let (mut entries, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_leaf_entries(page.data())?,
                    read_right_sibling(page.data()),
                )
            };
            if entries
                .first()
                .is_some_and(|first| first.key.as_slice() > key)
            {
                return Ok(false);
            }
            let Some(idx) = entries
                .iter()
                .position(|entry| entry.key.as_slice() == key && entry.value == value)
            else {
                if entries.last().is_some_and(|last| last.key.as_slice() > key) {
                    return Ok(false);
                }
                page_no = right_sibling;
                continue;
            };
            let old_first_key = entries.first().map(|entry| entry.key.clone());
            entries.remove(idx);
            let first_key_changed = old_first_key != entries.first().map(|entry| entry.key.clone());
            let should_unlink_empty_leaf =
                entries.is_empty() && !self.is_only_leaf(page_no, right_sibling)?;
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &entries)?;
            write_right_sibling(data, right_sibling)?;
            drop(page);
            drop(guard);
            if should_unlink_empty_leaf {
                self.unlink_empty_leaf(page_no, right_sibling)?;
            }
            self.refresh_leaf_directory_after_mutation(
                had_directory,
                first_key_changed || should_unlink_empty_leaf,
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn flush(&self) -> BufferPoolResult<()> {
        self.pool.flush_all()?;
        self.pool.sync()
    }

    pub fn stats(&self) -> BufferPoolResult<DiskVarBTreeStats> {
        let mut page_no = self.first_leaf()?;
        let mut stats = DiskVarBTreeStats {
            allocated_pages: self.page_count()?,
            free_leaf_pages: self.free_leaf_pages()?,
            internal_pages: self.internal_pages()?,
            ..DiskVarBTreeStats::default()
        };
        let mut previous: Option<VarEntry> = None;
        while page_no != NO_PAGE {
            let (entries, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_leaf_entries(page.data())?,
                    read_right_sibling(page.data()),
                )
            };
            stats.linked_leaf_pages += 1;
            if entries.is_empty() {
                stats.empty_leaf_pages += 1;
            }
            stats.live_entries += u64::try_from(entries.len())
                .map_err(|_| corrupt("var-btree entry count overflow"))?;
            stats.max_leaf_entries = stats.max_leaf_entries.max(
                u16::try_from(entries.len())
                    .map_err(|_| corrupt("var-btree leaf entry count exceeds u16"))?,
            );
            stats.payload_bytes += u64::try_from(leaf_payload_size(&entries)?)
                .map_err(|_| corrupt("var-btree payload size overflow"))?;
            for entry in entries {
                if previous.as_ref().is_some_and(|previous| {
                    (previous.key.as_slice(), previous.value) > (entry.key.as_slice(), entry.value)
                }) {
                    return Err(corrupt("var-btree leaf chain is not sorted"));
                }
                previous = Some(entry);
            }
            page_no = right_sibling;
        }
        Ok(stats)
    }

    pub fn rebuild_separator_directory(&self) -> BufferPoolResult<()> {
        self.rebuild_leaf_directory()
    }

    fn initialize_empty(&self) -> BufferPoolResult<()> {
        self.initialize_meta_page()?;
        let leaf = self.allocate_leaf_page()?;
        self.set_first_leaf(leaf)?;
        self.set_page_count(leaf.saturating_add(1))?;
        Ok(())
    }

    fn initialize_meta_page(&self) -> BufferPoolResult<()> {
        let meta = self.pool.new_page(self.config.relation_id)?;
        if meta.page_id().page_number != META_PAGE_NO {
            return Err(corrupt(
                "var-btree metapage allocation did not return page 0",
            ));
        }
        {
            let mut page = meta.write();
            let data = page.data_mut();
            data.fill(0);
            data[..META_MAGIC.len()].copy_from_slice(META_MAGIC);
            write_u64(data, META_FIRST_LEAF_OFFSET, NO_PAGE)?;
            write_u64(data, META_PAGE_COUNT_OFFSET, 1)?;
            write_u64(data, META_FREE_LIST_OFFSET, NO_PAGE)?;
            write_u64(data, META_FIRST_INTERNAL_OFFSET, NO_PAGE)?;
            write_u64(data, META_INTERNAL_LEVELS_OFFSET, 0)?;
        }
        Ok(())
    }

    fn append_leaf_page(&self, entries: &[VarEntry]) -> BufferPoolResult<u64> {
        let page_no = self.allocate_leaf_page()?;
        {
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            write_leaf_entries(page.data_mut(), entries)?;
        }
        if self.first_leaf()? == NO_PAGE {
            self.set_first_leaf(page_no)?;
        }
        Ok(page_no)
    }

    fn link_leaf_pages(&self) -> BufferPoolResult<()> {
        let page_count = self.page_count()?;
        if page_count <= 2 {
            return Ok(());
        }
        for page_no in 1..page_count - 1 {
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            write_right_sibling(page.data_mut(), page_no + 1)?;
        }
        Ok(())
    }

    fn allocate_leaf_page(&self) -> BufferPoolResult<u64> {
        if let Some(page_no) = self.pop_free_leaf_page()? {
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            let data = page.data_mut();
            data.fill(0);
            data[..LEAF_MAGIC.len()].copy_from_slice(LEAF_MAGIC);
            write_count(data, 0)?;
            write_right_sibling(data, NO_PAGE)?;
            return Ok(page_no);
        }

        let guard = self.pool.new_page(self.config.relation_id)?;
        let page_no = guard.page_id().page_number;
        {
            let mut page = guard.write();
            let data = page.data_mut();
            data.fill(0);
            data[..LEAF_MAGIC.len()].copy_from_slice(LEAF_MAGIC);
            write_count(data, 0)?;
            write_right_sibling(data, NO_PAGE)?;
        }
        self.set_page_count(
            page_no
                .saturating_add(1)
                .max(self.page_count().unwrap_or(0)),
        )?;
        Ok(page_no)
    }

    fn split_leaf(
        &self,
        page_no: u64,
        mut entries: Vec<VarEntry>,
        old_right: u64,
    ) -> BufferPoolResult<()> {
        let mut left = Vec::new();
        let mut left_size = LEAF_HEADER_SIZE;
        let target = leaf_payload_size(&entries)? / 2;
        while entries.len() > 1 {
            let next_size = entry_size(&entries[0])?;
            if !left.is_empty() && left_size + next_size > target {
                break;
            }
            left_size += next_size;
            left.push(entries.remove(0));
        }
        let right = entries;
        let right_page = self.allocate_leaf_page()?;

        {
            let guard = self.fetch_page(page_no)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &left)?;
            write_right_sibling(data, right_page)?;
        }
        {
            let guard = self.fetch_page(right_page)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &right)?;
            write_right_sibling(data, old_right)?;
        }
        Ok(())
    }

    fn fetch_page(&self, page_number: u64) -> BufferPoolResult<crate::pool::PageGuard<'_>> {
        self.pool.fetch_page(PageId {
            relation_id: self.config.relation_id,
            page_number,
        })
    }

    fn first_leaf(&self) -> BufferPoolResult<u64> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let page = guard.read();
        read_u64(page.data(), META_FIRST_LEAF_OFFSET)
    }

    fn page_count(&self) -> BufferPoolResult<u64> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let page = guard.read();
        read_u64(page.data(), META_PAGE_COUNT_OFFSET)
    }

    fn set_first_leaf(&self, page_no: u64) -> BufferPoolResult<()> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let mut page = guard.write();
        write_u64(page.data_mut(), META_FIRST_LEAF_OFFSET, page_no)
    }

    fn set_page_count(&self, page_count: u64) -> BufferPoolResult<()> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let mut page = guard.write();
        write_u64(page.data_mut(), META_PAGE_COUNT_OFFSET, page_count)
    }

    fn first_internal(&self) -> BufferPoolResult<u64> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let page = guard.read();
        let page_no = read_u64(page.data(), META_FIRST_INTERNAL_OFFSET)?;
        if page_no == META_PAGE_NO {
            Ok(NO_PAGE)
        } else {
            Ok(page_no)
        }
    }

    fn set_first_internal(&self, page_no: u64) -> BufferPoolResult<()> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let mut page = guard.write();
        write_u64(page.data_mut(), META_FIRST_INTERNAL_OFFSET, page_no)
    }

    fn internal_levels(&self) -> BufferPoolResult<u64> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let page = guard.read();
        let levels = read_u64(page.data(), META_INTERNAL_LEVELS_OFFSET)?;
        if levels == 0 && self.first_internal()? != NO_PAGE {
            Ok(1)
        } else {
            Ok(levels)
        }
    }

    fn set_internal_levels(&self, levels: u64) -> BufferPoolResult<()> {
        let guard = self.fetch_page(META_PAGE_NO)?;
        let mut page = guard.write();
        write_u64(page.data_mut(), META_INTERNAL_LEVELS_OFFSET, levels)
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

    fn pop_free_leaf_page(&self) -> BufferPoolResult<Option<u64>> {
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

    fn push_free_leaf_page(&self, page_no: u64) -> BufferPoolResult<()> {
        let old_head = self.free_list_head()?;
        let guard = self.fetch_page(page_no)?;
        let mut page = guard.write();
        let data = page.data_mut();
        data.fill(0);
        data[..LEAF_MAGIC.len()].copy_from_slice(LEAF_MAGIC);
        write_count(data, 0)?;
        write_right_sibling(data, old_head)?;
        drop(page);
        drop(guard);
        self.set_free_list_head(page_no)
    }

    fn free_leaf_pages(&self) -> BufferPoolResult<u64> {
        let page_count = self.page_count()?;
        let mut page_no = self.free_list_head()?;
        let mut count = 0_u64;
        while page_no != NO_PAGE {
            if page_no == META_PAGE_NO || page_no >= page_count {
                return Err(corrupt("var-btree free list points outside relation"));
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| corrupt("var-btree free list count overflow"))?;
            if count > page_count {
                return Err(corrupt("var-btree free list cycle detected"));
            }
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            page_no = read_right_sibling(page.data());
        }
        Ok(count)
    }

    fn leaf_for_lower_bound(&self, lower: &[u8]) -> BufferPoolResult<u64> {
        if let Some(page_no) = self.directory_leaf_for_lower_bound(lower)? {
            return Ok(page_no);
        }
        let mut page_no = self.first_leaf()?;
        let mut previous = page_no;
        while page_no != NO_PAGE {
            let (first, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_first_key(page.data())?,
                    read_right_sibling(page.data()),
                )
            };
            let Some(first) = first else {
                return Ok(page_no);
            };
            if first.as_slice() > lower {
                return Ok(previous);
            }
            previous = page_no;
            page_no = right_sibling;
        }
        Ok(previous)
    }

    fn directory_leaf_for_lower_bound(&self, lower: &[u8]) -> BufferPoolResult<Option<u64>> {
        let mut page_no = self.first_internal()?;
        if page_no == NO_PAGE {
            return Ok(None);
        }
        let levels = self.internal_levels()?;
        for _ in 0..levels {
            let Some(child_page_no) = self.directory_child_for_lower_bound(page_no, lower)? else {
                return Ok(None);
            };
            page_no = child_page_no;
        }
        Ok(Some(page_no))
    }

    fn directory_child_for_lower_bound(
        &self,
        mut page_no: u64,
        lower: &[u8],
    ) -> BufferPoolResult<Option<u64>> {
        let mut candidate = None;
        while page_no != NO_PAGE {
            let (entries, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_internal_entries(page.data())?,
                    read_internal_right_sibling(page.data()),
                )
            };
            for entry in entries {
                if entry.key.as_slice() > lower {
                    return Ok(Some(candidate.unwrap_or(entry.value)));
                }
                candidate = Some(entry.value);
            }
            page_no = right_sibling;
        }
        Ok(candidate)
    }

    fn rebuild_leaf_directory(&self) -> BufferPoolResult<()> {
        let mut separators = Vec::new();
        let mut page_no = self.first_leaf()?;
        while page_no != NO_PAGE {
            let (first, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (
                    read_first_key(page.data())?,
                    read_right_sibling(page.data()),
                )
            };
            if let Some(key) = first {
                separators.push(VarEntry {
                    key,
                    value: page_no,
                });
            }
            page_no = right_sibling;
        }
        self.invalidate_leaf_directory()?;
        if separators.len() <= 1 {
            self.set_internal_levels(0)?;
            return Ok(());
        }

        let mut levels = 1_u64;
        let mut parent_entries = self.write_internal_level(&separators)?;
        while parent_entries.len() > 1 {
            parent_entries = self.write_internal_level(&parent_entries)?;
            levels = levels
                .checked_add(1)
                .ok_or_else(|| corrupt("var-btree internal level count overflow"))?;
        }
        self.set_first_internal(parent_entries[0].value)?;
        self.set_internal_levels(levels)
    }

    fn write_internal_level(&self, entries: &[VarEntry]) -> BufferPoolResult<Vec<VarEntry>> {
        let mut page_entries = Vec::new();
        let mut current_size = INTERNAL_HEADER_SIZE;
        let mut parent_entries = Vec::new();
        for entry in entries {
            let entry_size = entry_size(&entry)?;
            if entry_size > PAGE_SIZE - INTERNAL_HEADER_SIZE {
                return Err(corrupt(
                    "var-btree separator key does not fit on internal page",
                ));
            }
            if !page_entries.is_empty() && current_size + entry_size > PAGE_SIZE {
                let page_no = self.append_internal_page(&page_entries)?;
                parent_entries.push(VarEntry {
                    key: page_entries[0].key.clone(),
                    value: page_no,
                });
                page_entries.clear();
                current_size = INTERNAL_HEADER_SIZE;
            }
            current_size += entry_size;
            page_entries.push(entry.clone());
        }
        if !page_entries.is_empty() {
            let page_no = self.append_internal_page(&page_entries)?;
            parent_entries.push(VarEntry {
                key: page_entries[0].key.clone(),
                value: page_no,
            });
        }
        for window in parent_entries.windows(2) {
            let guard = self.fetch_page(window[0].value)?;
            let mut page = guard.write();
            write_internal_right_sibling(page.data_mut(), window[1].value)?;
        }
        Ok(parent_entries)
    }

    fn refresh_leaf_directory_after_mutation(
        &self,
        had_directory: bool,
        directory_key_changed: bool,
    ) -> BufferPoolResult<()> {
        if had_directory && directory_key_changed {
            self.rebuild_leaf_directory()
        } else if had_directory {
            Ok(())
        } else {
            self.invalidate_leaf_directory()
        }
    }

    fn append_internal_page(&self, entries: &[VarEntry]) -> BufferPoolResult<u64> {
        let page_no = self.allocate_internal_page()?;
        let guard = self.fetch_page(page_no)?;
        let mut page = guard.write();
        let data = page.data_mut();
        write_internal_entries(data, entries)?;
        write_internal_right_sibling(data, NO_PAGE)?;
        drop(page);
        drop(guard);
        Ok(page_no)
    }

    fn allocate_internal_page(&self) -> BufferPoolResult<u64> {
        if let Some(page_no) = self.pop_free_leaf_page()? {
            return Ok(page_no);
        }

        let guard = self.pool.new_page(self.config.relation_id)?;
        let page_no = guard.page_id().page_number;
        self.set_page_count(
            page_no
                .saturating_add(1)
                .max(self.page_count().unwrap_or(0)),
        )?;
        Ok(page_no)
    }

    fn invalidate_leaf_directory(&self) -> BufferPoolResult<()> {
        let page_no = self.first_internal()?;
        if page_no == NO_PAGE {
            self.set_internal_levels(0)?;
            return Ok(());
        }
        self.set_first_internal(NO_PAGE)?;
        self.set_internal_levels(0)?;
        let page_count = self.page_count()?;
        let mut pending = vec![page_no];
        let mut reclaimed = 0_u64;
        while let Some(page_no) = pending.pop() {
            if page_no == NO_PAGE {
                continue;
            }
            if page_no == META_PAGE_NO || page_no >= page_count {
                return Err(corrupt(
                    "var-btree internal directory points outside relation",
                ));
            }
            reclaimed = reclaimed
                .checked_add(1)
                .ok_or_else(|| corrupt("var-btree internal directory count overflow"))?;
            if reclaimed > page_count {
                return Err(corrupt("var-btree internal directory cycle detected"));
            }
            let entries = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                read_internal_entries(page.data())?
            };
            for entry in entries {
                if entry.value != NO_PAGE && entry.value < page_count {
                    let guard = self.fetch_page(entry.value)?;
                    let page = guard.read();
                    if page.data()[..INTERNAL_MAGIC.len()] == *INTERNAL_MAGIC {
                        pending.push(entry.value);
                    }
                }
            }
            self.push_free_leaf_page(page_no)?;
        }
        Ok(())
    }

    fn internal_pages(&self) -> BufferPoolResult<u64> {
        let page_count = self.page_count()?;
        let mut count = 0_u64;
        for page_no in 1..page_count {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            if page.data()[..INTERNAL_MAGIC.len()] == *INTERNAL_MAGIC {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| corrupt("var-btree internal directory count overflow"))?;
            }
        }
        Ok(count)
    }

    fn leaf_for_insert(&self, key: &[u8]) -> BufferPoolResult<u64> {
        let mut page_no = self.first_leaf()?;
        while page_no != NO_PAGE {
            let (last, right_sibling) = {
                let guard = self.fetch_page(page_no)?;
                let page = guard.read();
                (read_last_key(page.data())?, read_right_sibling(page.data()))
            };
            if right_sibling == NO_PAGE || last.as_deref().map_or(true, |last| key <= last) {
                return Ok(page_no);
            }
            page_no = right_sibling;
        }
        Ok(NO_PAGE)
    }

    fn is_only_leaf(&self, page_no: u64, right_sibling: u64) -> BufferPoolResult<bool> {
        Ok(self.first_leaf()? == page_no && right_sibling == NO_PAGE)
    }

    fn unlink_empty_leaf(&self, page_no: u64, right_sibling: u64) -> BufferPoolResult<()> {
        let first = self.first_leaf()?;
        if first == page_no {
            self.set_first_leaf(right_sibling)?;
            return self.push_free_leaf_page(page_no);
        }

        let mut previous = first;
        while previous != NO_PAGE {
            let previous_right = {
                let guard = self.fetch_page(previous)?;
                let page = guard.read();
                read_right_sibling(page.data())
            };
            if previous_right == page_no {
                let guard = self.fetch_page(previous)?;
                let mut page = guard.write();
                write_right_sibling(page.data_mut(), right_sibling)?;
                drop(page);
                drop(guard);
                return self.push_free_leaf_page(page_no);
            }
            previous = previous_right;
        }
        Err(corrupt(
            "var-btree empty leaf is not linked from leaf chain",
        ))
    }
}

fn read_leaf_entries(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<Vec<VarEntry>> {
    if data[..LEAF_MAGIC.len()] != *LEAF_MAGIC {
        return Err(corrupt("var-btree leaf has invalid magic"));
    }
    read_entries(data, read_count(data)?, LEAF_HEADER_SIZE)
}

fn read_internal_entries(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<Vec<VarEntry>> {
    if data[..INTERNAL_MAGIC.len()] != *INTERNAL_MAGIC {
        return Err(corrupt("var-btree internal page has invalid magic"));
    }
    read_entries(data, read_internal_count(data)?, INTERNAL_HEADER_SIZE)
}

fn read_entries(
    data: &[u8; PAGE_SIZE],
    count: usize,
    mut offset: usize,
) -> BufferPoolResult<Vec<VarEntry>> {
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if offset + ENTRY_HEADER_SIZE > PAGE_SIZE {
            return Err(corrupt("var-btree entry header is truncated"));
        }
        let key_len = usize::from(read_u16(data, offset)?);
        let value = read_u64(data, offset + 2)?;
        offset += ENTRY_HEADER_SIZE;
        if offset + key_len > PAGE_SIZE {
            return Err(corrupt("var-btree key bytes are truncated"));
        }
        entries.push(VarEntry {
            key: data[offset..offset + key_len].to_vec(),
            value,
        });
        offset += key_len;
    }
    Ok(entries)
}

fn read_first_key(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<Option<Vec<u8>>> {
    if read_count(data)? == 0 {
        return Ok(None);
    }
    read_key_at(data, LEAF_HEADER_SIZE).map(Some)
}

fn read_last_key(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<Option<Vec<u8>>> {
    let count = read_count(data)?;
    if count == 0 {
        return Ok(None);
    }
    let mut offset = LEAF_HEADER_SIZE;
    let mut key = None;
    for _ in 0..count {
        let current = read_key_at(data, offset)?;
        let key_len = current.len();
        offset += ENTRY_HEADER_SIZE + key_len;
        key = Some(current);
    }
    Ok(key)
}

fn read_key_at(data: &[u8; PAGE_SIZE], offset: usize) -> BufferPoolResult<Vec<u8>> {
    if offset + ENTRY_HEADER_SIZE > PAGE_SIZE {
        return Err(corrupt("var-btree key header is truncated"));
    }
    let key_len = usize::from(read_u16(data, offset)?);
    let key_offset = offset + ENTRY_HEADER_SIZE;
    if key_offset + key_len > PAGE_SIZE {
        return Err(corrupt("var-btree key bytes are truncated"));
    }
    Ok(data[key_offset..key_offset + key_len].to_vec())
}

fn write_leaf_entries(data: &mut [u8; PAGE_SIZE], entries: &[VarEntry]) -> BufferPoolResult<()> {
    data.fill(0);
    data[..LEAF_MAGIC.len()].copy_from_slice(LEAF_MAGIC);
    write_count(data, entries.len())?;
    write_right_sibling(data, NO_PAGE)?;
    write_entries(data, entries, LEAF_HEADER_SIZE)
}

fn write_internal_entries(
    data: &mut [u8; PAGE_SIZE],
    entries: &[VarEntry],
) -> BufferPoolResult<()> {
    data.fill(0);
    data[..INTERNAL_MAGIC.len()].copy_from_slice(INTERNAL_MAGIC);
    write_internal_count(data, entries.len())?;
    write_internal_right_sibling(data, NO_PAGE)?;
    write_entries(data, entries, INTERNAL_HEADER_SIZE)
}

fn write_entries(
    data: &mut [u8; PAGE_SIZE],
    entries: &[VarEntry],
    mut offset: usize,
) -> BufferPoolResult<()> {
    for entry in entries {
        let key_len = u16::try_from(entry.key.len())
            .map_err(|_| corrupt("var-btree key length exceeds u16"))?;
        if offset + ENTRY_HEADER_SIZE + entry.key.len() > PAGE_SIZE {
            return Err(corrupt("var-btree leaf entries exceed page size"));
        }
        write_u16(data, offset, key_len)?;
        write_u64(data, offset + 2, entry.value)?;
        offset += ENTRY_HEADER_SIZE;
        data[offset..offset + entry.key.len()].copy_from_slice(&entry.key);
        offset += entry.key.len();
    }
    Ok(())
}

fn entry_size(entry: &VarEntry) -> BufferPoolResult<usize> {
    entry_size_for_key(&entry.key)
}

fn entry_size_for_key(key: &[u8]) -> BufferPoolResult<usize> {
    ENTRY_HEADER_SIZE
        .checked_add(key.len())
        .ok_or_else(|| corrupt("var-btree entry size overflow"))
}

fn leaf_payload_size(entries: &[VarEntry]) -> BufferPoolResult<usize> {
    entries.iter().try_fold(LEAF_HEADER_SIZE, |acc, entry| {
        acc.checked_add(entry_size(entry)?)
            .ok_or_else(|| corrupt("var-btree leaf size overflow"))
    })
}

fn validate_sorted_entries(entries: &[VarEntry]) -> BufferPoolResult<()> {
    for window in entries.windows(2) {
        let left = &window[0];
        let right = &window[1];
        if (left.key.as_slice(), left.value) > (right.key.as_slice(), right.value) {
            return Err(corrupt("var-btree bulk_load_sorted input is not sorted"));
        }
    }
    Ok(())
}

fn read_count(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<usize> {
    Ok(usize::from(read_u16(data, LEAF_COUNT_OFFSET)?))
}

fn write_count(data: &mut [u8; PAGE_SIZE], count: usize) -> BufferPoolResult<()> {
    let count = u16::try_from(count).map_err(|_| corrupt("var-btree entry count exceeds u16"))?;
    write_u16(data, LEAF_COUNT_OFFSET, count)
}

fn read_internal_count(data: &[u8; PAGE_SIZE]) -> BufferPoolResult<usize> {
    Ok(usize::from(read_u16(data, INTERNAL_COUNT_OFFSET)?))
}

fn write_internal_count(data: &mut [u8; PAGE_SIZE], count: usize) -> BufferPoolResult<()> {
    let count = u16::try_from(count).map_err(|_| corrupt("var-btree entry count exceeds u16"))?;
    write_u16(data, INTERNAL_COUNT_OFFSET, count)
}

fn read_right_sibling(data: &[u8; PAGE_SIZE]) -> u64 {
    read_u64(data, LEAF_RIGHT_SIBLING_OFFSET).unwrap_or(NO_PAGE)
}

fn write_right_sibling(data: &mut [u8; PAGE_SIZE], page_no: u64) -> BufferPoolResult<()> {
    write_u64(data, LEAF_RIGHT_SIBLING_OFFSET, page_no)
}

fn read_internal_right_sibling(data: &[u8; PAGE_SIZE]) -> u64 {
    read_u64(data, INTERNAL_RIGHT_SIBLING_OFFSET).unwrap_or(NO_PAGE)
}

fn write_internal_right_sibling(data: &mut [u8; PAGE_SIZE], page_no: u64) -> BufferPoolResult<()> {
    write_u64(data, INTERNAL_RIGHT_SIBLING_OFFSET, page_no)
}

fn read_u16(data: &[u8; PAGE_SIZE], offset: usize) -> BufferPoolResult<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| corrupt("var-btree u16 read out of bounds"))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn write_u16(data: &mut [u8; PAGE_SIZE], offset: usize, value: u16) -> BufferPoolResult<()> {
    let dst = data
        .get_mut(offset..offset + 2)
        .ok_or_else(|| corrupt("var-btree u16 write out of bounds"))?;
    dst.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_u64(data: &[u8; PAGE_SIZE], offset: usize) -> BufferPoolResult<u64> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or_else(|| corrupt("var-btree u64 read out of bounds"))?;
    let mut out = [0u8; 8];
    out.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(out))
}

fn write_u64(data: &mut [u8; PAGE_SIZE], offset: usize, value: u64) -> BufferPoolResult<()> {
    let dst = data
        .get_mut(offset..offset + 8)
        .ok_or_else(|| corrupt("var-btree u64 write out of bounds"))?;
    dst.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn corrupt(message: impl Into<String>) -> BufferPoolError {
    BufferPoolError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

#[cfg(test)]
#[path = "disk_var_btree_tests.rs"]
mod tests;
