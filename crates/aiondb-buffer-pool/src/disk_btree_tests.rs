use super::*;
use crate::pool::MemoryPageStore;

fn tree_with_caps(
    leaf_capacity: usize,
    internal_capacity: usize,
) -> (Arc<MemoryPageStore>, DiskBTree) {
    let store = Arc::new(MemoryPageStore::new());
    let pool = Arc::new(BufferPool::new(64, store.clone()));
    let tree = DiskBTree::open_or_create(
        pool,
        DiskBTreeConfig {
            relation_id: 42,
            leaf_capacity,
            internal_capacity,
        },
    )
    .unwrap();
    (store, tree)
}

#[test]
fn inserts_and_gets_single_leaf_entries() {
    let (_store, tree) = tree_with_caps(4, 4);
    tree.insert(10, 100).unwrap();
    tree.insert(2, 20).unwrap();
    tree.insert(7, 70).unwrap();

    assert_eq!(tree.get(2).unwrap(), Some(20));
    assert_eq!(tree.get(7).unwrap(), Some(70));
    assert_eq!(tree.get(10).unwrap(), Some(100));
    assert_eq!(tree.get(11).unwrap(), None);
    assert_eq!(tree.stats().unwrap().height, 1);
}

#[test]
fn updates_existing_key_without_duplicate() {
    let (_store, tree) = tree_with_caps(4, 4);
    tree.insert(1, 10).unwrap();
    tree.insert(1, 99).unwrap();

    assert_eq!(tree.get(1).unwrap(), Some(99));
    assert_eq!(tree.stats().unwrap().height, 1);
}

#[test]
fn splits_leaf_and_grows_root() {
    let (_store, tree) = tree_with_caps(3, 4);
    for key in 0..10 {
        tree.insert(key, key * 10).unwrap();
    }
    for key in 0..10 {
        assert_eq!(tree.get(key).unwrap(), Some(key * 10));
    }
    assert!(tree.stats().unwrap().height >= 2);
}

#[test]
fn splits_internal_pages() {
    let (_store, tree) = tree_with_caps(3, 3);
    for key in 0..100 {
        tree.insert(key, key + 1000).unwrap();
    }
    for key in 0..100 {
        assert_eq!(tree.get(key).unwrap(), Some(key + 1000));
    }
    assert!(tree.stats().unwrap().height >= 3);
}

#[test]
fn range_scan_walks_leaf_siblings_in_key_order() {
    let (_store, tree) = tree_with_caps(3, 3);
    for key in (0..30).rev() {
        tree.insert(key, key * 10).unwrap();
    }

    let rows = tree.range(Some(7), Some(16), None).unwrap();
    assert_eq!(
        rows,
        (7..=16).map(|key| (key, key * 10)).collect::<Vec<_>>()
    );

    let limited = tree.range(Some(12), None, Some(4)).unwrap();
    assert_eq!(
        limited,
        (12..16).map(|key| (key, key * 10)).collect::<Vec<_>>()
    );
}

#[test]
fn delete_removes_key_without_rebalancing() {
    let (_store, tree) = tree_with_caps(3, 3);
    for key in 0..20 {
        tree.insert(key, key * 10).unwrap();
    }

    assert!(tree.delete(7).unwrap());
    assert!(!tree.delete(7).unwrap());
    assert_eq!(tree.get(7).unwrap(), None);
    assert_eq!(tree.get(8).unwrap(), Some(80));
    assert_eq!(
        tree.range(Some(5), Some(9), None).unwrap(),
        vec![(5, 50), (6, 60), (8, 80), (9, 90)]
    );
}

#[test]
fn delete_compacts_empty_rightmost_leaf() {
    let (_store, tree) = tree_with_caps(3, 3);
    for key in 0..6 {
        tree.insert(key, key * 10).unwrap();
    }
    let before = tree.stats().unwrap();
    assert!(before.page_count >= 5);

    assert!(tree.delete(4).unwrap());
    assert!(tree.delete(5).unwrap());

    let after = tree.stats().unwrap();
    assert!(after.page_count < before.page_count);
    assert_eq!(tree.get(4).unwrap(), None);
    assert_eq!(tree.get(5).unwrap(), None);
    assert_eq!(
        tree.range(Some(0), Some(3), None).unwrap(),
        vec![(0, 0), (1, 10), (2, 20), (3, 30)]
    );
}

#[test]
fn delete_can_shrink_single_internal_root() {
    let (_store, tree) = tree_with_caps(3, 3);
    for key in 0..4 {
        tree.insert(key, key * 10).unwrap();
    }
    assert!(tree.stats().unwrap().height >= 2);

    assert!(tree.delete(2).unwrap());
    assert!(tree.delete(3).unwrap());

    let stats = tree.stats().unwrap();
    assert_eq!(stats.height, 1);
    assert_eq!(tree.get(0).unwrap(), Some(0));
    assert_eq!(tree.get(1).unwrap(), Some(10));
    assert_eq!(tree.get(2).unwrap(), None);
    assert_eq!(tree.get(3).unwrap(), None);
}

#[test]
fn delete_can_collapse_two_leaf_root_when_combined_payload_fits() {
    let (_store, tree) = tree_with_caps(4, 8);
    for key in 0..6 {
        tree.insert(key, key * 10).unwrap();
    }
    assert!(tree.stats().unwrap().height >= 2);

    assert!(tree.delete(4).unwrap());
    assert!(tree.delete(5).unwrap());

    let stats = tree.stats().unwrap();
    assert_eq!(stats.height, 1);
    assert_eq!(
        tree.range(Some(0), Some(3), None).unwrap(),
        vec![(0, 0), (1, 10), (2, 20), (3, 30)]
    );
}

#[test]
fn delete_can_collapse_two_internal_root_when_combined_payload_fits() {
    let (_store, tree) = tree_with_caps(2, 4);
    for key in 0..24 {
        tree.insert(key, key * 10).unwrap();
    }
    let before = tree.stats().unwrap();
    assert!(before.height >= 3);

    for key in 0..12 {
        assert!(tree.delete(key).unwrap());
    }

    let after = tree.stats().unwrap();
    assert!(after.height < before.height);
    for key in 12..24 {
        assert_eq!(tree.get(key).unwrap(), Some(key * 10));
    }
    assert_eq!(
        tree.range(Some(12), Some(23), None).unwrap(),
        (12..=23).map(|key| (key, key * 10)).collect::<Vec<_>>()
    );
}

#[test]
fn delete_handles_iterative_root_collapse_opportunities_without_breakage() {
    let (_store, tree) = tree_with_caps(4, 4);
    for key in 0..24 {
        tree.insert(key, key * 10).unwrap();
    }
    let before = tree.stats().unwrap();
    assert!(before.height >= 3);

    for key in 8..24 {
        assert!(tree.delete(key).unwrap());
    }

    let after = tree.stats().unwrap();
    assert!(after.height <= before.height);
    assert_eq!(
        tree.range(Some(0), Some(7), None).unwrap(),
        (0..=7).map(|key| (key, key * 10)).collect::<Vec<_>>()
    );
}

#[test]
fn delete_compacts_empty_leftmost_leaf() {
    let (_store, tree) = tree_with_caps(2, 3);
    for key in 0..6 {
        tree.insert(key, key * 10).unwrap();
    }

    assert!(tree.delete(0).unwrap());
    assert!(tree.delete(1).unwrap());

    assert_eq!(tree.get(0).unwrap(), None);
    assert_eq!(tree.get(1).unwrap(), None);
    assert_eq!(
        tree.range(Some(2), Some(5), None).unwrap(),
        vec![(2, 20), (3, 30), (4, 40), (5, 50)]
    );
}

#[test]
fn delete_compacts_empty_middle_leaf() {
    let (_store, tree) = tree_with_caps(2, 3);
    for key in 0..8 {
        tree.insert(key, key * 10).unwrap();
    }

    assert!(tree.delete(2).unwrap());
    assert!(tree.delete(3).unwrap());

    assert_eq!(tree.get(2).unwrap(), None);
    assert_eq!(tree.get(3).unwrap(), None);
    assert_eq!(
        tree.range(Some(0), Some(7), None).unwrap(),
        vec![(0, 0), (1, 10), (4, 40), (5, 50), (6, 60), (7, 70)]
    );
}

#[test]
fn delete_refreshes_parent_separator_when_leaf_first_key_changes() {
    let (_store, tree) = tree_with_caps(3, 8);
    for key in 0..6 {
        tree.insert(key, key * 10).unwrap();
    }

    assert!(tree.delete(2).unwrap());

    assert_eq!(tree.get(2).unwrap(), None);
    assert_eq!(tree.get(3).unwrap(), Some(30));
    assert_eq!(tree.get(4).unwrap(), Some(40));
    assert_eq!(
        tree.range(Some(0), Some(5), None).unwrap(),
        vec![(0, 0), (1, 10), (3, 30), (4, 40), (5, 50)]
    );
}

#[test]
fn delete_series_preserves_remaining_keys_in_deeper_tree() {
    let (_store, tree) = tree_with_caps(2, 2);
    for key in 0..12 {
        tree.insert(key, key * 10).unwrap();
    }
    let before = tree.stats().unwrap();
    assert!(before.height >= 3);

    for key in 0..4 {
        assert!(tree.delete(key).unwrap());
    }

    let after = tree.stats().unwrap();
    assert!(after.height <= before.height);
    assert_eq!(
        tree.range(Some(4), Some(11), None).unwrap(),
        (4..=11).map(|key| (key, key * 10)).collect::<Vec<_>>()
    );
}

#[test]
fn delete_propagates_new_min_when_leftmost_child_subtree_changes() {
    let (_store, tree) = tree_with_caps(2, 2);
    for key in 0..12 {
        tree.insert(key, key * 10).unwrap();
    }

    for key in 0..4 {
        assert!(tree.delete(key).unwrap());
    }

    for key in 4..12 {
        assert_eq!(tree.get(key).unwrap(), Some(key * 10));
    }
    assert_eq!(
        tree.range(Some(4), Some(11), None).unwrap(),
        (4..=11).map(|key| (key, key * 10)).collect::<Vec<_>>()
    );
}

#[test]
fn delete_propagates_new_min_after_leftmost_internal_collapse() {
    let (_store, tree) = tree_with_caps(2, 2);
    for key in 0..20 {
        tree.insert(key, key * 10).unwrap();
    }
    assert!(tree.stats().unwrap().height >= 3);

    for key in 0..8 {
        assert!(tree.delete(key).unwrap());
    }

    for key in 8..20 {
        assert_eq!(tree.get(key).unwrap(), Some(key * 10));
    }
    assert_eq!(
        tree.range(Some(8), Some(19), None).unwrap(),
        (8..=19).map(|key| (key, key * 10)).collect::<Vec<_>>()
    );
}

#[test]
fn delete_rebalances_deep_internal_pages_without_breaking_search() {
    let (_store, tree) = tree_with_caps(2, 3);
    for key in 0..30 {
        tree.insert(key, key * 10).unwrap();
    }
    assert!(tree.stats().unwrap().height >= 3);

    for key in 0..10 {
        assert!(tree.delete(key).unwrap());
    }

    for key in 10..30 {
        assert_eq!(tree.get(key).unwrap(), Some(key * 10));
    }
    assert_eq!(
        tree.range(Some(10), Some(29), None).unwrap(),
        (10..=29).map(|key| (key, key * 10)).collect::<Vec<_>>()
    );
}

#[test]
fn delete_merges_deep_internal_pages_without_breaking_search() {
    let (_store, tree) = tree_with_caps(2, 4);
    for key in 0..32 {
        tree.insert(key, key * 10).unwrap();
    }
    let before = tree.stats().unwrap();
    assert!(before.height >= 3);

    for key in 0..14 {
        assert!(tree.delete(key).unwrap());
    }

    let after = tree.stats().unwrap();
    assert!(after.height <= before.height);
    for key in 14..32 {
        assert_eq!(tree.get(key).unwrap(), Some(key * 10));
    }
    assert_eq!(
        tree.range(Some(14), Some(31), None).unwrap(),
        (14..=31).map(|key| (key, key * 10)).collect::<Vec<_>>()
    );
}

#[test]
fn freed_middle_leaf_page_is_reused_by_later_split() {
    let (_store, tree) = tree_with_caps(2, 10);
    for key in 0..8 {
        tree.insert(key, key * 10).unwrap();
    }
    let before_delete = tree.stats().unwrap();

    assert!(tree.delete(2).unwrap());
    assert!(tree.delete(3).unwrap());

    let after_delete = tree.stats().unwrap();
    assert_eq!(after_delete.page_count, before_delete.page_count);

    tree.insert(100, 1000).unwrap();
    tree.insert(101, 1010).unwrap();

    let after_reuse = tree.stats().unwrap();
    assert_eq!(after_reuse.page_count, before_delete.page_count);
    assert_eq!(tree.get(100).unwrap(), Some(1000));
    assert_eq!(tree.get(101).unwrap(), Some(1010));
}

#[test]
fn delete_merges_leaf_with_right_sibling_when_combined_entries_fit() {
    let (_store, tree) = tree_with_caps(3, 8);
    for key in 0..6 {
        tree.insert(key, key * 10).unwrap();
    }
    let before = tree.stats().unwrap();

    assert!(tree.delete(3).unwrap());

    let after = tree.stats().unwrap();
    assert!(after.page_count <= before.page_count);
    assert_eq!(
        tree.range(Some(0), Some(5), None).unwrap(),
        vec![(0, 0), (1, 10), (2, 20), (4, 40), (5, 50)]
    );

    tree.insert(100, 1000).unwrap();

    let after_reuse = tree.stats().unwrap();
    assert!(after_reuse.page_count >= after.page_count);
    assert_eq!(tree.get(100).unwrap(), Some(1000));
}

#[test]
fn delete_merges_leaf_with_left_sibling_when_combined_entries_fit() {
    let (_store, tree) = tree_with_caps(3, 8);
    for key in 0..6 {
        tree.insert(key, key * 10).unwrap();
    }
    let before = tree.stats().unwrap();

    assert!(tree.delete(2).unwrap());

    let after = tree.stats().unwrap();
    assert!(after.page_count <= before.page_count);
    assert_eq!(
        tree.range(Some(0), Some(5), None).unwrap(),
        vec![(0, 0), (1, 10), (3, 30), (4, 40), (5, 50)]
    );
}

#[test]
fn delete_redistributes_with_right_sibling_when_merge_would_not_fit() {
    let (_store, tree) = tree_with_caps(4, 8);
    for key in 0..8 {
        tree.insert(key, key * 10).unwrap();
    }

    assert!(tree.delete(2).unwrap());

    assert_eq!(tree.get(2).unwrap(), None);
    assert_eq!(tree.get(3).unwrap(), Some(30));
    assert_eq!(tree.get(4).unwrap(), Some(40));
    assert_eq!(
        tree.range(Some(0), Some(7), None).unwrap(),
        vec![(0, 0), (1, 10), (3, 30), (4, 40), (5, 50), (6, 60), (7, 70)]
    );
}

#[test]
fn delete_redistributes_with_left_sibling_when_merge_would_not_fit() {
    let (_store, tree) = tree_with_caps(4, 8);
    for key in 0..8 {
        tree.insert(key, key * 10).unwrap();
    }

    assert!(tree.delete(5).unwrap());

    assert_eq!(tree.get(5).unwrap(), None);
    assert_eq!(tree.get(4).unwrap(), Some(40));
    assert_eq!(tree.get(6).unwrap(), Some(60));
    assert_eq!(
        tree.range(Some(0), Some(7), None).unwrap(),
        vec![(0, 0), (1, 10), (2, 20), (3, 30), (4, 40), (6, 60), (7, 70)]
    );
}

#[test]
fn survives_reopen_with_same_page_store() {
    let store = Arc::new(MemoryPageStore::new());
    {
        let pool = Arc::new(BufferPool::new(8, store.clone()));
        let tree = DiskBTree::open_or_create(pool, DiskBTreeConfig::new(7)).unwrap();
        for key in 0..50 {
            tree.insert(key, key * 2).unwrap();
        }
        tree.flush().unwrap();
    }
    {
        let pool = Arc::new(BufferPool::new(8, store.clone()));
        let tree = DiskBTree::open_or_create(pool, DiskBTreeConfig::new(7)).unwrap();
        for key in 0..50 {
            assert_eq!(tree.get(key).unwrap(), Some(key * 2));
        }
    }
}

#[test]
fn reset_relation_rebuilds_from_page_zero_without_stale_keys() {
    let store = Arc::new(MemoryPageStore::new());
    let pool = Arc::new(BufferPool::new(8, store.clone()));
    {
        let tree = DiskBTree::open_or_create(pool.clone(), DiskBTreeConfig::new(7)).unwrap();
        tree.insert(10, 100).unwrap();
        tree.flush().unwrap();
    }

    pool.reset_relation(7).unwrap();

    {
        let tree = DiskBTree::open_or_create(pool, DiskBTreeConfig::new(7)).unwrap();
        assert_eq!(tree.get(10).unwrap(), None);
        tree.insert(20, 200).unwrap();
        assert_eq!(tree.get(20).unwrap(), Some(200));
    }
}
