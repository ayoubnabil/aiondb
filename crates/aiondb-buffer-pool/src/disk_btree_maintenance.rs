#![allow(clippy::wildcard_imports)]

use super::*;

impl DiskBTree {
    pub(super) fn compact_empty_leaf(
        &self,
        leaf_page_no: u64,
        right_sibling: u64,
        path: &[PathFrame],
    ) -> BufferPoolResult<()> {
        let Some(parent_frame) = path.last().copied() else {
            return Ok(());
        };
        let parent_guard = self.fetch_page(parent_frame.page_no)?;
        let parent_page = parent_guard.read();
        let first_child = read_first_child(parent_page.data());
        let mut entries = read_internal_entries(parent_page.data())?;
        drop(parent_page);
        drop(parent_guard);

        let child_count = entries.len().saturating_add(1);
        if parent_frame.child_slot >= child_count {
            return Ok(());
        }

        let next_child = if parent_frame.child_slot < entries.len() {
            Some(entries[parent_frame.child_slot].1)
        } else {
            None
        };

        let previous_leaf = self.find_previous_leaf(leaf_page_no)?;
        if let Some(previous_leaf) = previous_leaf {
            let previous_guard = self.fetch_page(previous_leaf)?;
            let mut previous_page = previous_guard.write();
            write_right_sibling(previous_page.data_mut(), right_sibling);
        }

        let new_first_child = if parent_frame.child_slot == 0 {
            let Some(next_child) = next_child else {
                return Ok(());
            };
            entries.remove(0);
            next_child
        } else {
            entries.remove(parent_frame.child_slot - 1);
            first_child
        };
        {
            let parent_guard = self.fetch_page(parent_frame.page_no)?;
            let mut parent_page = parent_guard.write();
            write_internal_entries(parent_page.data_mut(), new_first_child, &entries)?;
        }
        if parent_frame.child_slot == 0 && !path.is_empty() {
            let new_min = self.subtree_first_key(new_first_child)?;
            self.refresh_ancestor_separators_after_child_min_change(
                &path[..path.len().saturating_sub(1)],
                new_min,
            )?;
        }
        self.try_redistribute_deepest_internal(path)?;

        let mut removed_pages = vec![leaf_page_no];
        self.collapse_single_child_internal_path(path, &mut removed_pages)?;
        self.collapse_root_single_child_chain(&mut removed_pages)?;
        self.reclaim_removed_pages(&removed_pages)?;
        Ok(())
    }

    pub(super) fn try_merge_leaf_with_right_sibling(
        &self,
        leaf_page_no: u64,
        leaf_entries: &[(u64, u64)],
        right_sibling: u64,
        path: &[PathFrame],
    ) -> BufferPoolResult<bool> {
        let Some(parent_frame) = path.last().copied() else {
            return Ok(false);
        };
        if right_sibling == NO_PAGE {
            return Ok(false);
        }

        let parent_guard = self.fetch_page(parent_frame.page_no)?;
        let parent_page = parent_guard.read();
        let first_child = read_first_child(parent_page.data());
        let mut parent_entries = read_internal_entries(parent_page.data())?;
        drop(parent_page);
        drop(parent_guard);

        if parent_frame.child_slot >= parent_entries.len() {
            return Ok(false);
        }
        if parent_entries[parent_frame.child_slot].1 != right_sibling {
            return Ok(false);
        }

        let (mut right_entries, right_right_sibling) = {
            let guard = self.fetch_page(right_sibling)?;
            let page = guard.read();
            (
                read_leaf_entries(page.data())?,
                read_right_sibling(page.data()),
            )
        };
        if leaf_entries.len().saturating_add(right_entries.len()) > self.config.leaf_capacity {
            return Ok(false);
        }

        let mut merged = leaf_entries.to_vec();
        merged.append(&mut right_entries);
        {
            let guard = self.fetch_page(leaf_page_no)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &merged)?;
            write_right_sibling(data, right_right_sibling);
        }

        parent_entries.remove(parent_frame.child_slot);
        {
            let parent_guard = self.fetch_page(parent_frame.page_no)?;
            let mut parent_page = parent_guard.write();
            write_internal_entries(parent_page.data_mut(), first_child, &parent_entries)?;
        }
        self.try_redistribute_deepest_internal(path)?;

        let mut removed_pages = vec![right_sibling];
        self.collapse_single_child_internal_path(path, &mut removed_pages)?;
        self.collapse_root_single_child_chain(&mut removed_pages)?;
        self.reclaim_removed_pages(&removed_pages)?;
        Ok(true)
    }

    pub(super) fn try_merge_leaf_with_left_sibling(
        &self,
        leaf_page_no: u64,
        path: &[PathFrame],
    ) -> BufferPoolResult<bool> {
        let Some(parent_frame) = path.last().copied() else {
            return Ok(false);
        };
        if parent_frame.child_slot == 0 {
            return Ok(false);
        }

        let parent_guard = self.fetch_page(parent_frame.page_no)?;
        let parent_page = parent_guard.read();
        let first_child = read_first_child(parent_page.data());
        let mut parent_entries = read_internal_entries(parent_page.data())?;
        drop(parent_page);
        drop(parent_guard);

        let left_sibling = if parent_frame.child_slot == 1 {
            first_child
        } else {
            let idx = parent_frame.child_slot - 2;
            if idx >= parent_entries.len() {
                return Ok(false);
            }
            parent_entries[idx].1
        };

        let (mut left_entries, left_right_sibling) = {
            let guard = self.fetch_page(left_sibling)?;
            let page = guard.read();
            (
                read_leaf_entries(page.data())?,
                read_right_sibling(page.data()),
            )
        };
        if left_right_sibling != leaf_page_no {
            return Ok(false);
        }

        let (current_entries, current_right_sibling) = {
            let guard = self.fetch_page(leaf_page_no)?;
            let page = guard.read();
            (
                read_leaf_entries(page.data())?,
                read_right_sibling(page.data()),
            )
        };
        if left_entries.len().saturating_add(current_entries.len()) > self.config.leaf_capacity {
            return Ok(false);
        }

        left_entries.extend_from_slice(&current_entries);
        {
            let guard = self.fetch_page(left_sibling)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &left_entries)?;
            write_right_sibling(data, current_right_sibling);
        }

        parent_entries.remove(parent_frame.child_slot - 1);
        {
            let parent_guard = self.fetch_page(parent_frame.page_no)?;
            let mut parent_page = parent_guard.write();
            write_internal_entries(parent_page.data_mut(), first_child, &parent_entries)?;
        }
        self.try_redistribute_deepest_internal(path)?;

        let mut removed_pages = vec![leaf_page_no];
        self.collapse_single_child_internal_path(path, &mut removed_pages)?;
        self.collapse_root_single_child_chain(&mut removed_pages)?;
        self.reclaim_removed_pages(&removed_pages)?;
        Ok(true)
    }

    pub(super) fn try_redistribute_leaf_with_right_sibling(
        &self,
        leaf_page_no: u64,
        leaf_entries: &[(u64, u64)],
        right_sibling: u64,
        path: &[PathFrame],
    ) -> BufferPoolResult<bool> {
        let Some(parent_frame) = path.last().copied() else {
            return Ok(false);
        };
        if right_sibling == NO_PAGE {
            return Ok(false);
        }
        let parent_guard = self.fetch_page(parent_frame.page_no)?;
        let parent_page = parent_guard.read();
        let first_child = read_first_child(parent_page.data());
        let mut parent_entries = read_internal_entries(parent_page.data())?;
        drop(parent_page);
        drop(parent_guard);
        if parent_frame.child_slot >= parent_entries.len() {
            return Ok(false);
        }
        if parent_entries[parent_frame.child_slot].1 != right_sibling {
            return Ok(false);
        }

        let (mut right_entries, right_right_sibling) = {
            let guard = self.fetch_page(right_sibling)?;
            let page = guard.read();
            (
                read_leaf_entries(page.data())?,
                read_right_sibling(page.data()),
            )
        };
        let total = leaf_entries.len().saturating_add(right_entries.len());
        if total <= self.config.leaf_capacity {
            return Ok(false);
        }
        let target_left = total / 2;
        if leaf_entries.len() >= target_left || right_entries.is_empty() {
            return Ok(false);
        }

        let mut left_entries = leaf_entries.to_vec();
        while left_entries.len() < target_left && !right_entries.is_empty() {
            left_entries.push(right_entries.remove(0));
        }
        if right_entries.is_empty() {
            return Ok(false);
        }

        {
            let guard = self.fetch_page(leaf_page_no)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &left_entries)?;
            write_right_sibling(data, right_sibling);
        }
        {
            let guard = self.fetch_page(right_sibling)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &right_entries)?;
            write_right_sibling(data, right_right_sibling);
        }

        parent_entries[parent_frame.child_slot].0 = right_entries[0].0;
        let parent_guard = self.fetch_page(parent_frame.page_no)?;
        let mut parent_page = parent_guard.write();
        write_internal_entries(parent_page.data_mut(), first_child, &parent_entries)?;
        Ok(true)
    }

    pub(super) fn try_redistribute_leaf_with_left_sibling(
        &self,
        leaf_page_no: u64,
        path: &[PathFrame],
    ) -> BufferPoolResult<bool> {
        let Some(parent_frame) = path.last().copied() else {
            return Ok(false);
        };
        if parent_frame.child_slot == 0 {
            return Ok(false);
        }
        let parent_guard = self.fetch_page(parent_frame.page_no)?;
        let parent_page = parent_guard.read();
        let first_child = read_first_child(parent_page.data());
        let mut parent_entries = read_internal_entries(parent_page.data())?;
        drop(parent_page);
        drop(parent_guard);

        let left_sibling = if parent_frame.child_slot == 1 {
            first_child
        } else {
            let idx = parent_frame.child_slot - 2;
            if idx >= parent_entries.len() {
                return Ok(false);
            }
            parent_entries[idx].1
        };
        let (mut left_entries, left_right_sibling) = {
            let guard = self.fetch_page(left_sibling)?;
            let page = guard.read();
            (
                read_leaf_entries(page.data())?,
                read_right_sibling(page.data()),
            )
        };
        if left_right_sibling != leaf_page_no {
            return Ok(false);
        }
        let (mut current_entries, current_right_sibling) = {
            let guard = self.fetch_page(leaf_page_no)?;
            let page = guard.read();
            (
                read_leaf_entries(page.data())?,
                read_right_sibling(page.data()),
            )
        };
        let total = left_entries.len().saturating_add(current_entries.len());
        if total <= self.config.leaf_capacity {
            return Ok(false);
        }
        let target_left = total / 2;
        if left_entries.len() <= target_left || current_entries.is_empty() {
            return Ok(false);
        }

        while left_entries.len() > target_left {
            let moved = left_entries
                .pop()
                .ok_or_else(|| corrupt("btree left redistribution underflow"))?;
            current_entries.insert(0, moved);
        }
        if current_entries.is_empty() {
            return Ok(false);
        }

        {
            let guard = self.fetch_page(left_sibling)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &left_entries)?;
            write_right_sibling(data, leaf_page_no);
        }
        {
            let guard = self.fetch_page(leaf_page_no)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &current_entries)?;
            write_right_sibling(data, current_right_sibling);
        }

        parent_entries[parent_frame.child_slot - 1].0 = current_entries[0].0;
        let parent_guard = self.fetch_page(parent_frame.page_no)?;
        let mut parent_page = parent_guard.write();
        write_internal_entries(parent_page.data_mut(), first_child, &parent_entries)?;
        Ok(true)
    }

    pub(super) fn find_previous_leaf(&self, target_page_no: u64) -> BufferPoolResult<Option<u64>> {
        let mut previous = None;
        let mut page_no = self.leftmost_leaf()?;
        while page_no != NO_PAGE {
            if page_no == target_page_no {
                return Ok(previous);
            }
            previous = Some(page_no);
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            page_no = read_right_sibling(page.data());
        }
        Ok(None)
    }

    pub(super) fn try_redistribute_deepest_internal(
        &self,
        path: &[PathFrame],
    ) -> BufferPoolResult<bool> {
        if path.len() < 2 {
            return Ok(false);
        }
        let current = path[path.len() - 1];
        let parent = path[path.len() - 2];
        let current_guard = self.fetch_page(current.page_no)?;
        let current_page = current_guard.read();
        let current_first_child = read_first_child(current_page.data());
        let current_entries = read_internal_entries(current_page.data())?;
        drop(current_page);
        drop(current_guard);

        if current_first_child == NO_PAGE || current_entries.is_empty() {
            return Ok(false);
        }
        if current_entries.len() >= self.min_internal_entries() {
            return Ok(false);
        }

        if self.try_merge_internal_with_right_sibling(current, parent, path)? {
            return Ok(true);
        }
        if self.try_merge_internal_with_left_sibling(current, parent, path)? {
            return Ok(true);
        }
        if self.try_redistribute_internal_with_right_sibling(current, parent)? {
            return Ok(true);
        }
        self.try_redistribute_internal_with_left_sibling(current, parent)
    }

    pub(super) fn try_merge_internal_with_right_sibling(
        &self,
        current: PathFrame,
        parent: PathFrame,
        path: &[PathFrame],
    ) -> BufferPoolResult<bool> {
        let parent_guard = self.fetch_page(parent.page_no)?;
        let parent_page = parent_guard.read();
        let parent_first_child = read_first_child(parent_page.data());
        let mut parent_entries = read_internal_entries(parent_page.data())?;
        drop(parent_page);
        drop(parent_guard);

        if parent.child_slot >= parent_entries.len() {
            return Ok(false);
        }
        let right_page_no = parent_entries[parent.child_slot].1;

        let current_guard = self.fetch_page(current.page_no)?;
        let current_page = current_guard.read();
        let current_first_child = read_first_child(current_page.data());
        let mut current_entries = read_internal_entries(current_page.data())?;
        drop(current_page);
        drop(current_guard);

        let right_guard = self.fetch_page(right_page_no)?;
        let right_page = right_guard.read();
        let right_first_child = read_first_child(right_page.data());
        let right_entries = read_internal_entries(right_page.data())?;
        drop(right_page);
        drop(right_guard);

        let merged_len = current_entries
            .len()
            .saturating_add(1)
            .saturating_add(right_entries.len());
        if merged_len > self.config.internal_capacity {
            return Ok(false);
        }

        let parent_separator = parent_entries[parent.child_slot].0;
        current_entries.push((parent_separator, right_first_child));
        current_entries.extend_from_slice(&right_entries);

        {
            let guard = self.fetch_page(current.page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), current_first_child, &current_entries)?;
        }

        parent_entries.remove(parent.child_slot);
        {
            let guard = self.fetch_page(parent.page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), parent_first_child, &parent_entries)?;
        }

        self.try_redistribute_deepest_internal(&path[..path.len() - 1])?;
        let mut removed_pages = vec![right_page_no];
        self.collapse_single_child_internal_path(path, &mut removed_pages)?;
        self.collapse_root_single_child_chain(&mut removed_pages)?;
        self.reclaim_removed_pages(&removed_pages)?;
        Ok(true)
    }

    pub(super) fn try_merge_internal_with_left_sibling(
        &self,
        current: PathFrame,
        parent: PathFrame,
        path: &[PathFrame],
    ) -> BufferPoolResult<bool> {
        if parent.child_slot == 0 {
            return Ok(false);
        }
        let parent_guard = self.fetch_page(parent.page_no)?;
        let parent_page = parent_guard.read();
        let parent_first_child = read_first_child(parent_page.data());
        let mut parent_entries = read_internal_entries(parent_page.data())?;
        drop(parent_page);
        drop(parent_guard);

        let left_page_no = if parent.child_slot == 1 {
            parent_first_child
        } else {
            let idx = parent.child_slot - 2;
            if idx >= parent_entries.len() {
                return Ok(false);
            }
            parent_entries[idx].1
        };

        let left_guard = self.fetch_page(left_page_no)?;
        let left_page = left_guard.read();
        let left_first_child = read_first_child(left_page.data());
        let mut left_entries = read_internal_entries(left_page.data())?;
        drop(left_page);
        drop(left_guard);

        let current_guard = self.fetch_page(current.page_no)?;
        let current_page = current_guard.read();
        let current_first_child = read_first_child(current_page.data());
        let current_entries = read_internal_entries(current_page.data())?;
        drop(current_page);
        drop(current_guard);

        let merged_len = left_entries
            .len()
            .saturating_add(1)
            .saturating_add(current_entries.len());
        if merged_len > self.config.internal_capacity {
            return Ok(false);
        }

        let parent_separator = parent_entries[parent.child_slot - 1].0;
        left_entries.push((parent_separator, current_first_child));
        left_entries.extend_from_slice(&current_entries);

        {
            let guard = self.fetch_page(left_page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), left_first_child, &left_entries)?;
        }

        parent_entries.remove(parent.child_slot - 1);
        {
            let guard = self.fetch_page(parent.page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), parent_first_child, &parent_entries)?;
        }

        self.try_redistribute_deepest_internal(&path[..path.len() - 1])?;
        let mut removed_pages = vec![current.page_no];
        self.collapse_single_child_internal_path(path, &mut removed_pages)?;
        self.collapse_root_single_child_chain(&mut removed_pages)?;
        self.reclaim_removed_pages(&removed_pages)?;
        Ok(true)
    }

    pub(super) fn try_redistribute_internal_with_right_sibling(
        &self,
        current: PathFrame,
        parent: PathFrame,
    ) -> BufferPoolResult<bool> {
        let parent_guard = self.fetch_page(parent.page_no)?;
        let parent_page = parent_guard.read();
        let parent_first_child = read_first_child(parent_page.data());
        let mut parent_entries = read_internal_entries(parent_page.data())?;
        drop(parent_page);
        drop(parent_guard);

        if parent.child_slot >= parent_entries.len() {
            return Ok(false);
        }
        let right_page_no = parent_entries[parent.child_slot].1;

        let current_guard = self.fetch_page(current.page_no)?;
        let current_page = current_guard.read();
        let current_first_child = read_first_child(current_page.data());
        let mut current_entries = read_internal_entries(current_page.data())?;
        drop(current_page);
        drop(current_guard);

        let right_guard = self.fetch_page(right_page_no)?;
        let right_page = right_guard.read();
        let mut right_first_child = read_first_child(right_page.data());
        let mut right_entries = read_internal_entries(right_page.data())?;
        drop(right_page);
        drop(right_guard);

        if right_entries.is_empty() {
            return Ok(false);
        }

        let total_children = current_entries.len() + 1 + right_entries.len() + 1;
        let target_left_children = total_children / 2;
        let target_left_entries = target_left_children.saturating_sub(1);
        if current_entries.len() >= target_left_entries {
            return Ok(false);
        }

        while current_entries.len() < target_left_entries && !right_entries.is_empty() {
            let parent_separator = parent_entries[parent.child_slot].0;
            current_entries.push((parent_separator, right_first_child));
            let promoted = right_entries.remove(0);
            right_first_child = promoted.1;
            parent_entries[parent.child_slot].0 = promoted.0;
        }

        {
            let guard = self.fetch_page(current.page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), current_first_child, &current_entries)?;
        }
        {
            let guard = self.fetch_page(right_page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), right_first_child, &right_entries)?;
        }
        {
            let guard = self.fetch_page(parent.page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), parent_first_child, &parent_entries)?;
        }
        Ok(true)
    }

    pub(super) fn try_redistribute_internal_with_left_sibling(
        &self,
        current: PathFrame,
        parent: PathFrame,
    ) -> BufferPoolResult<bool> {
        if parent.child_slot == 0 {
            return Ok(false);
        }
        let parent_guard = self.fetch_page(parent.page_no)?;
        let parent_page = parent_guard.read();
        let parent_first_child = read_first_child(parent_page.data());
        let mut parent_entries = read_internal_entries(parent_page.data())?;
        drop(parent_page);
        drop(parent_guard);

        let left_page_no = if parent.child_slot == 1 {
            parent_first_child
        } else {
            let idx = parent.child_slot - 2;
            if idx >= parent_entries.len() {
                return Ok(false);
            }
            parent_entries[idx].1
        };

        let left_guard = self.fetch_page(left_page_no)?;
        let left_page = left_guard.read();
        let left_first_child = read_first_child(left_page.data());
        let mut left_entries = read_internal_entries(left_page.data())?;
        drop(left_page);
        drop(left_guard);

        let current_guard = self.fetch_page(current.page_no)?;
        let current_page = current_guard.read();
        let mut current_first_child = read_first_child(current_page.data());
        let mut current_entries = read_internal_entries(current_page.data())?;
        drop(current_page);
        drop(current_guard);

        if left_entries.is_empty() {
            return Ok(false);
        }

        let total_children = left_entries.len() + 1 + current_entries.len() + 1;
        let target_left_children = total_children / 2;
        if left_entries.len() < target_left_children {
            return Ok(false);
        }

        while left_entries.len() + 1 > target_left_children {
            let moved = left_entries
                .pop()
                .ok_or_else(|| corrupt("btree internal left redistribution underflow"))?;
            let parent_separator = parent_entries[parent.child_slot - 1].0;
            current_entries.insert(0, (parent_separator, current_first_child));
            current_first_child = moved.1;
            parent_entries[parent.child_slot - 1].0 = moved.0;
        }

        {
            let guard = self.fetch_page(left_page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), left_first_child, &left_entries)?;
        }
        {
            let guard = self.fetch_page(current.page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), current_first_child, &current_entries)?;
        }
        {
            let guard = self.fetch_page(parent.page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), parent_first_child, &parent_entries)?;
        }
        Ok(true)
    }

    pub(super) fn collapse_single_child_internal_path(
        &self,
        path: &[PathFrame],
        removed_pages: &mut Vec<u64>,
    ) -> BufferPoolResult<()> {
        if path.len() < 2 {
            return Ok(());
        }
        for idx in (1..path.len()).rev() {
            let frame = path[idx];
            let guard = self.fetch_page(frame.page_no)?;
            let page = guard.read();
            let first_child = read_first_child(page.data());
            let entries = read_internal_entries(page.data())?;
            drop(page);
            drop(guard);

            if !entries.is_empty() || first_child == NO_PAGE {
                break;
            }

            let parent = path[idx - 1];
            let parent_guard = self.fetch_page(parent.page_no)?;
            let parent_page = parent_guard.read();
            let mut parent_first_child = read_first_child(parent_page.data());
            let mut parent_entries = read_internal_entries(parent_page.data())?;
            drop(parent_page);
            drop(parent_guard);

            if parent.child_slot == 0 {
                parent_first_child = first_child;
            } else if parent.child_slot - 1 < parent_entries.len() {
                parent_entries[parent.child_slot - 1].1 = first_child;
            } else {
                break;
            }

            let parent_guard = self.fetch_page(parent.page_no)?;
            let mut parent_page = parent_guard.write();
            write_internal_entries(parent_page.data_mut(), parent_first_child, &parent_entries)?;
            drop(parent_page);
            drop(parent_guard);
            if parent.child_slot == 0 && idx >= 1 {
                let new_min = self.subtree_first_key(parent_first_child)?;
                self.refresh_ancestor_separators_after_child_min_change(&path[..idx - 1], new_min)?;
            }
            removed_pages.push(frame.page_no);
        }
        Ok(())
    }

    pub(super) fn collapse_root_single_child_chain(
        &self,
        removed_pages: &mut Vec<u64>,
    ) -> BufferPoolResult<()> {
        loop {
            let current_height = self.height()?;
            if current_height <= 1 {
                return Ok(());
            }
            let root_page = self.root_page()?;
            let guard = self.fetch_page(root_page)?;
            let page = guard.read();
            match read_page_kind(page.data())? {
                PageKind::Leaf => return Ok(()),
                PageKind::Internal => {
                    let first_child = read_first_child(page.data());
                    let entries = read_internal_entries(page.data())?;
                    if !entries.is_empty() || first_child == NO_PAGE {
                        return Ok(());
                    }
                    removed_pages.push(root_page);
                    self.set_root(first_child, current_height - 1)?;
                }
            }
        }
    }

    pub(super) fn collapse_root_leaf_pair_if_possible(&self) -> BufferPoolResult<bool> {
        if self.height()? != 2 {
            return Ok(false);
        }
        let root_page = self.root_page()?;
        let root_guard = self.fetch_page(root_page)?;
        let root_page_guard = root_guard.read();
        if read_page_kind(root_page_guard.data())? != PageKind::Internal {
            return Ok(false);
        }
        let first_child = read_first_child(root_page_guard.data());
        let entries = read_internal_entries(root_page_guard.data())?;
        drop(root_page_guard);
        drop(root_guard);

        if entries.len() != 1 {
            return Ok(false);
        }
        let right_child = entries[0].1;

        let left_guard = self.fetch_page(first_child)?;
        let left_page = left_guard.read();
        if read_page_kind(left_page.data())? != PageKind::Leaf {
            return Ok(false);
        }
        let mut left_entries = read_leaf_entries(left_page.data())?;
        let left_right = read_right_sibling(left_page.data());
        drop(left_page);
        drop(left_guard);

        let right_guard = self.fetch_page(right_child)?;
        let right_page = right_guard.read();
        if read_page_kind(right_page.data())? != PageKind::Leaf {
            return Ok(false);
        }
        let right_entries = read_leaf_entries(right_page.data())?;
        let right_right = read_right_sibling(right_page.data());
        drop(right_page);
        drop(right_guard);

        if left_right != right_child {
            return Ok(false);
        }
        if left_entries.len().saturating_add(right_entries.len()) > self.config.leaf_capacity {
            return Ok(false);
        }

        left_entries.extend_from_slice(&right_entries);
        {
            let guard = self.fetch_page(first_child)?;
            let mut page = guard.write();
            let data = page.data_mut();
            write_leaf_entries(data, &left_entries)?;
            write_right_sibling(data, right_right);
        }
        self.set_root(first_child, 1)?;
        self.reclaim_removed_pages(&[right_child, root_page])?;
        Ok(true)
    }

    pub(super) fn collapse_root_pairs_if_possible(&self) -> BufferPoolResult<()> {
        loop {
            if self.collapse_root_internal_pair_if_possible()? {
                continue;
            }
            if self.collapse_root_leaf_pair_if_possible()? {
                continue;
            }
            return Ok(());
        }
    }

    pub(super) fn collapse_root_internal_pair_if_possible(&self) -> BufferPoolResult<bool> {
        let height = self.height()?;
        if height <= 2 {
            return Ok(false);
        }
        let root_page = self.root_page()?;
        let root_guard = self.fetch_page(root_page)?;
        let root_page_guard = root_guard.read();
        if read_page_kind(root_page_guard.data())? != PageKind::Internal {
            return Ok(false);
        }
        let root_first_child = read_first_child(root_page_guard.data());
        let root_entries = read_internal_entries(root_page_guard.data())?;
        drop(root_page_guard);
        drop(root_guard);

        if root_entries.len() != 1 {
            return Ok(false);
        }
        let right_child = root_entries[0].1;

        let left_guard = self.fetch_page(root_first_child)?;
        let left_page = left_guard.read();
        if read_page_kind(left_page.data())? != PageKind::Internal {
            return Ok(false);
        }
        let left_first_child = read_first_child(left_page.data());
        let mut left_entries = read_internal_entries(left_page.data())?;
        drop(left_page);
        drop(left_guard);

        let right_guard = self.fetch_page(right_child)?;
        let right_page = right_guard.read();
        if read_page_kind(right_page.data())? != PageKind::Internal {
            return Ok(false);
        }
        let right_first_child = read_first_child(right_page.data());
        let right_entries = read_internal_entries(right_page.data())?;
        drop(right_page);
        drop(right_guard);

        let merged_len = left_entries
            .len()
            .saturating_add(1)
            .saturating_add(right_entries.len());
        if merged_len > self.config.internal_capacity {
            return Ok(false);
        }

        left_entries.push((root_entries[0].0, right_first_child));
        left_entries.extend_from_slice(&right_entries);
        {
            let guard = self.fetch_page(root_first_child)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), left_first_child, &left_entries)?;
        }

        self.set_root(root_first_child, height - 1)?;
        self.reclaim_removed_pages(&[right_child, root_page])?;
        Ok(true)
    }

    pub(super) fn refresh_ancestor_separators_after_child_min_change(
        &self,
        path: &[PathFrame],
        new_min: u64,
    ) -> BufferPoolResult<()> {
        for frame in path.iter().rev() {
            if frame.child_slot == 0 {
                continue;
            }
            let guard = self.fetch_page(frame.page_no)?;
            let page = guard.read();
            let first_child = read_first_child(page.data());
            let mut entries = read_internal_entries(page.data())?;
            drop(page);
            drop(guard);

            let idx = frame.child_slot - 1;
            if idx >= entries.len() {
                return Ok(());
            }
            if entries[idx].0 == new_min {
                return Ok(());
            }
            entries[idx].0 = new_min;

            let guard = self.fetch_page(frame.page_no)?;
            let mut page = guard.write();
            write_internal_entries(page.data_mut(), first_child, &entries)?;
            return Ok(());
        }
        Ok(())
    }

    pub(super) fn subtree_first_key(&self, mut page_no: u64) -> BufferPoolResult<u64> {
        loop {
            let guard = self.fetch_page(page_no)?;
            let page = guard.read();
            match read_page_kind(page.data())? {
                PageKind::Leaf => {
                    let entries = read_leaf_entries(page.data())?;
                    return entries
                        .first()
                        .map(|(key, _)| *key)
                        .ok_or_else(|| corrupt("btree leaf is unexpectedly empty"));
                }
                PageKind::Internal => {
                    page_no = read_first_child(page.data());
                    if page_no == NO_PAGE {
                        return Err(corrupt("btree internal page has no first child"));
                    }
                }
            }
        }
    }

    pub(super) fn reclaim_removed_pages(&self, removed_pages: &[u64]) -> BufferPoolResult<()> {
        let mut removed_pages = removed_pages.to_vec();
        removed_pages.sort_unstable();
        removed_pages.dedup();

        let mut page_count = self.page_count()?;
        while page_count > 0 {
            let candidate = page_count - 1;
            if removed_pages.binary_search(&candidate).is_ok() {
                page_count -= 1;
            } else {
                break;
            }
        }
        self.set_page_count(page_count)?;

        for page_no in removed_pages {
            if page_no == META_PAGE_NO || page_no >= page_count {
                continue;
            }
            self.push_free_page(page_no)?;
        }
        Ok(())
    }
}
