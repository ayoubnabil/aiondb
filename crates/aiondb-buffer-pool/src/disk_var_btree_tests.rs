use super::*;
use crate::{BufferPool, MemoryPageStore};

fn tree() -> (Arc<MemoryPageStore>, DiskVarBTree) {
    let store = Arc::new(MemoryPageStore::new());
    let pool = Arc::new(BufferPool::new(16, store.clone()));
    let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(42)).unwrap();
    (store, tree)
}

#[test]
fn bulk_load_and_range_scan_variable_keys() {
    let (_store, tree) = tree();
    tree.bulk_load_sorted(&[
        VarEntry {
            key: b"alpha".to_vec(),
            value: 1,
        },
        VarEntry {
            key: b"beta".to_vec(),
            value: 2,
        },
        VarEntry {
            key: b"betamax".to_vec(),
            value: 3,
        },
        VarEntry {
            key: b"zeta".to_vec(),
            value: 4,
        },
    ])
    .unwrap();

    let values = tree.range(Some(b"beta"), Some(b"betaz"), None).unwrap();
    assert_eq!(
        values,
        vec![
            VarEntry {
                key: b"beta".to_vec(),
                value: 2
            },
            VarEntry {
                key: b"betamax".to_vec(),
                value: 3
            },
        ]
    );
}

#[test]
fn exact_lookup_returns_all_values_for_variable_key() {
    let (_store, tree) = tree();
    tree.bulk_load_sorted(&[
        VarEntry {
            key: b"alpha".to_vec(),
            value: 1,
        },
        VarEntry {
            key: b"beta".to_vec(),
            value: 2,
        },
        VarEntry {
            key: b"beta".to_vec(),
            value: 3,
        },
        VarEntry {
            key: b"gamma".to_vec(),
            value: 4,
        },
    ])
    .unwrap();

    assert_eq!(tree.get_values(b"beta").unwrap(), vec![2, 3]);
    assert!(tree.get_values(b"delta").unwrap().is_empty());
}

#[test]
fn bulk_load_rejects_unsorted_input() {
    let (_store, tree) = tree();
    let err = tree
        .bulk_load_sorted(&[
            VarEntry {
                key: b"beta".to_vec(),
                value: 2,
            },
            VarEntry {
                key: b"alpha".to_vec(),
                value: 1,
            },
        ])
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("bulk_load_sorted input is not sorted"));
}

#[test]
fn exact_lookup_survives_reopen_and_uses_separator_directory() {
    let store = Arc::new(MemoryPageStore::new());
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(91)).unwrap();
        let mut entries = (0..1_200)
            .map(|idx| VarEntry {
                key: format!("k{idx:04}").into_bytes(),
                value: idx,
            })
            .collect::<Vec<_>>();
        entries.push(VarEntry {
            key: b"k0999".to_vec(),
            value: 99_999,
        });
        entries.sort_by(|left, right| {
            (left.key.as_slice(), left.value).cmp(&(right.key.as_slice(), right.value))
        });
        tree.bulk_load_sorted(&entries).unwrap();
        assert!(tree.stats().unwrap().internal_pages > 0);
        tree.flush().unwrap();
    }
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(91)).unwrap();
        assert!(tree.stats().unwrap().internal_pages > 0);
        assert_eq!(tree.get_values(b"k0999").unwrap(), vec![999, 99_999]);
    }
}

#[test]
fn survives_reopen() {
    let store = Arc::new(MemoryPageStore::new());
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(77)).unwrap();
        tree.bulk_load_sorted(&[
            VarEntry {
                key: b"a".to_vec(),
                value: 10,
            },
            VarEntry {
                key: b"b".to_vec(),
                value: 20,
            },
        ])
        .unwrap();
        tree.flush().unwrap();
    }
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(77)).unwrap();
        assert_eq!(
            tree.range(Some(b"b"), Some(b"b"), None).unwrap(),
            vec![VarEntry {
                key: b"b".to_vec(),
                value: 20
            }]
        );
    }
}

#[test]
fn spans_multiple_leaf_pages() {
    let (_store, tree) = tree();
    let entries = (0..600)
        .map(|idx| VarEntry {
            key: format!("k{idx:04}").into_bytes(),
            value: idx,
        })
        .collect::<Vec<_>>();
    tree.bulk_load_sorted(&entries).unwrap();

    let out = tree.range(Some(b"k0100"), Some(b"k0103"), None).unwrap();
    assert_eq!(out.len(), 4);
    assert_eq!(out[0].value, 100);
    assert_eq!(out[3].value, 103);
}

#[test]
fn online_insert_preserves_key_order() {
    let (_store, tree) = tree();
    for (key, value) in [(b"gamma".as_slice(), 3), (b"alpha", 1), (b"beta", 2)] {
        tree.insert(key.to_vec(), value).unwrap();
    }

    let out = tree.range(None, None, None).unwrap();
    assert_eq!(
        out,
        vec![
            VarEntry {
                key: b"alpha".to_vec(),
                value: 1
            },
            VarEntry {
                key: b"beta".to_vec(),
                value: 2
            },
            VarEntry {
                key: b"gamma".to_vec(),
                value: 3
            },
        ]
    );
}

#[test]
fn online_insert_splits_leaf_chain() {
    let (_store, tree) = tree();
    for idx in (0..900).rev() {
        tree.insert(format!("k{idx:04}").into_bytes(), idx).unwrap();
    }

    let out = tree.range(Some(b"k0448"), Some(b"k0452"), None).unwrap();
    assert_eq!(out.len(), 5);
    assert_eq!(out[0].value, 448);
    assert_eq!(out[4].value, 452);
}

#[test]
fn range_seek_starts_near_lower_bound_after_splits() {
    let (_store, tree) = tree();
    for idx in 0..1200 {
        tree.insert(format!("k{idx:04}").into_bytes(), idx).unwrap();
    }

    let out = tree.range(Some(b"k0990"), Some(b"k0993"), Some(2)).unwrap();
    assert_eq!(
        out,
        vec![
            VarEntry {
                key: b"k0990".to_vec(),
                value: 990
            },
            VarEntry {
                key: b"k0991".to_vec(),
                value: 991
            },
        ]
    );
}

#[test]
fn online_insert_survives_reopen() {
    let store = Arc::new(MemoryPageStore::new());
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(88)).unwrap();
        tree.insert(b"delta".to_vec(), 4).unwrap();
        tree.insert(b"alpha".to_vec(), 1).unwrap();
        tree.flush().unwrap();
    }
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(88)).unwrap();
        assert_eq!(
            tree.range(None, None, None).unwrap(),
            vec![
                VarEntry {
                    key: b"alpha".to_vec(),
                    value: 1
                },
                VarEntry {
                    key: b"delta".to_vec(),
                    value: 4
                },
            ]
        );
    }
}

#[test]
fn delete_removes_exact_entry_without_touching_neighbors() {
    let (_store, tree) = tree();
    for (key, value) in [
        (b"alpha".as_slice(), 1),
        (b"beta", 2),
        (b"beta", 3),
        (b"gamma", 4),
    ] {
        tree.insert(key.to_vec(), value).unwrap();
    }

    assert!(tree.delete(b"beta", 2).unwrap());
    assert!(!tree.delete(b"beta", 99).unwrap());
    assert_eq!(
        tree.range(Some(b"beta"), Some(b"beta"), None).unwrap(),
        vec![VarEntry {
            key: b"beta".to_vec(),
            value: 3
        }]
    );
}

#[test]
fn delete_survives_reopen() {
    let store = Arc::new(MemoryPageStore::new());
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(89)).unwrap();
        tree.insert(b"a".to_vec(), 1).unwrap();
        tree.insert(b"b".to_vec(), 2).unwrap();
        assert!(tree.delete(b"a", 1).unwrap());
        tree.flush().unwrap();
    }
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(89)).unwrap();
        assert_eq!(
            tree.range(None, None, None).unwrap(),
            vec![VarEntry {
                key: b"b".to_vec(),
                value: 2
            }]
        );
    }
}

#[test]
fn delete_unlinks_empty_leaf_from_scan_path() {
    let (_store, tree) = tree();
    for idx in 0..900 {
        tree.insert(format!("k{idx:04}").into_bytes(), idx).unwrap();
    }
    for idx in 0..300 {
        assert!(tree.delete(format!("k{idx:04}").as_bytes(), idx).unwrap());
    }

    let out = tree.range(Some(b"k0298"), Some(b"k0302"), None).unwrap();
    assert_eq!(
        out,
        vec![
            VarEntry {
                key: b"k0300".to_vec(),
                value: 300
            },
            VarEntry {
                key: b"k0301".to_vec(),
                value: 301
            },
            VarEntry {
                key: b"k0302".to_vec(),
                value: 302
            },
        ]
    );
}

#[test]
fn stats_reports_linked_leaf_chain_shape() {
    let (_store, tree) = tree();
    for idx in 0..900 {
        tree.insert(format!("k{idx:04}").into_bytes(), idx).unwrap();
    }
    for idx in 0..120 {
        assert!(tree.delete(format!("k{idx:04}").as_bytes(), idx).unwrap());
    }

    let stats = tree.stats().unwrap();
    assert_eq!(stats.live_entries, 780);
    assert!(stats.allocated_pages >= 2);
    assert!(stats.linked_leaf_pages >= 1);
    assert_eq!(stats.empty_leaf_pages, 0);
    assert!(stats.payload_bytes >= LEAF_HEADER_SIZE as u64);
    assert!(stats.max_leaf_entries > 0);
}

#[test]
fn empty_leaf_pages_are_reused_after_delete_churn() {
    let (_store, tree) = tree();
    for idx in 0..1800 {
        tree.insert(format!("k{idx:04}").into_bytes(), idx).unwrap();
    }
    for idx in 0..900 {
        assert!(tree.delete(format!("k{idx:04}").as_bytes(), idx).unwrap());
    }

    let after_delete = tree.stats().unwrap();
    assert!(after_delete.free_leaf_pages > 0);
    let allocated_before_reuse = after_delete.allocated_pages;

    for idx in 1800..2700 {
        tree.insert(format!("k{idx:04}").into_bytes(), idx).unwrap();
    }

    let after_reuse = tree.stats().unwrap();
    assert_eq!(after_reuse.live_entries, 1800);
    assert!(after_reuse.free_leaf_pages < after_delete.free_leaf_pages);
    assert!(after_reuse.allocated_pages <= allocated_before_reuse + 1);
}

#[test]
fn bulk_load_builds_persistent_internal_separator_directory() {
    let store = Arc::new(MemoryPageStore::new());
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(90)).unwrap();
        let entries = (0..1_200)
            .map(|idx| VarEntry {
                key: format!("k{idx:04}").into_bytes(),
                value: idx,
            })
            .collect::<Vec<_>>();
        tree.bulk_load_sorted(&entries).unwrap();
        let stats = tree.stats().unwrap();
        assert!(stats.internal_pages > 0);
        tree.flush().unwrap();
    }
    {
        let pool = Arc::new(BufferPool::new(16, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(90)).unwrap();
        assert!(tree.stats().unwrap().internal_pages > 0);
        let out = tree.range(Some(b"k0998"), Some(b"k1001"), None).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].value, 998);
        assert_eq!(out[3].value, 1001);
    }
}

#[test]
fn bulk_load_builds_recursive_internal_separator_levels() {
    let store = Arc::new(MemoryPageStore::new());
    {
        let pool = Arc::new(BufferPool::new(64, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(92)).unwrap();
        let suffix = "x".repeat(140);
        let entries = (0..5_000)
            .map(|idx| VarEntry {
                key: format!("k{idx:05}-{suffix}").into_bytes(),
                value: idx,
            })
            .collect::<Vec<_>>();
        tree.bulk_load_sorted(&entries).unwrap();
        let stats = tree.stats().unwrap();
        assert!(stats.internal_pages > 1);
        assert!(tree.internal_levels().unwrap() > 1);
        tree.flush().unwrap();
    }
    {
        let pool = Arc::new(BufferPool::new(64, store.clone()));
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(92)).unwrap();
        assert!(tree.stats().unwrap().internal_pages > 1);
        assert!(tree.internal_levels().unwrap() > 1);
        let suffix = "x".repeat(140);
        let lower = format!("k04497-{suffix}");
        let upper = format!("k04501-{suffix}");
        let out = tree
            .range(Some(lower.as_bytes()), Some(upper.as_bytes()), None)
            .unwrap();
        assert_eq!(out.len(), 5);
        assert_eq!(out[0].value, 4497);
        assert_eq!(out[4].value, 4501);
    }
}

#[test]
fn online_mutation_refreshes_existing_separator_directory() {
    let (_store, tree) = tree();
    let entries = (0..1_200)
        .map(|idx| VarEntry {
            key: format!("k{idx:04}").into_bytes(),
            value: idx,
        })
        .collect::<Vec<_>>();
    tree.bulk_load_sorted(&entries).unwrap();
    let before = tree.stats().unwrap();
    assert!(before.internal_pages > 0);

    tree.insert(b"k1200".to_vec(), 1200).unwrap();
    let after = tree.stats().unwrap();
    assert!(after.internal_pages > 0);
    assert_eq!(after.allocated_pages, before.allocated_pages);
    let out = tree.range(Some(b"k1199"), Some(b"k1200"), None).unwrap();
    assert_eq!(
        out,
        vec![
            VarEntry {
                key: b"k1199".to_vec(),
                value: 1199
            },
            VarEntry {
                key: b"k1200".to_vec(),
                value: 1200
            },
        ]
    );
}

#[test]
fn online_insert_inside_leaf_keeps_separator_directory_without_rebuild() {
    let (_store, tree) = tree();
    let entries = (0..1_200)
        .map(|idx| VarEntry {
            key: format!("k{idx:04}").into_bytes(),
            value: idx,
        })
        .collect::<Vec<_>>();
    tree.bulk_load_sorted(&entries).unwrap();
    let before = tree.stats().unwrap();
    assert!(before.internal_pages > 0);

    assert!(tree.delete(b"k0601", 601).unwrap());
    let after_delete = tree.stats().unwrap();
    assert_eq!(after_delete.internal_pages, before.internal_pages);
    assert_eq!(after_delete.allocated_pages, before.allocated_pages);

    tree.insert(b"k0601-extra".to_vec(), 99_999).unwrap();
    let after = tree.stats().unwrap();
    assert_eq!(after.internal_pages, before.internal_pages);
    assert_eq!(after.allocated_pages, before.allocated_pages);
    assert_eq!(after.free_leaf_pages, before.free_leaf_pages);

    let out = tree.range(Some(b"k0600"), Some(b"k0602"), None).unwrap();
    assert_eq!(
        out,
        vec![
            VarEntry {
                key: b"k0600".to_vec(),
                value: 600
            },
            VarEntry {
                key: b"k0601-extra".to_vec(),
                value: 99_999
            },
            VarEntry {
                key: b"k0602".to_vec(),
                value: 602
            },
        ]
    );
}

#[test]
fn explicit_separator_directory_rebuild_reuses_existing_pages() {
    let (_store, tree) = tree();
    let entries = (0..1_200)
        .map(|idx| VarEntry {
            key: format!("k{idx:04}").into_bytes(),
            value: idx,
        })
        .collect::<Vec<_>>();
    tree.bulk_load_sorted(&entries).unwrap();
    let before = tree.stats().unwrap();
    assert!(before.internal_pages > 0);
    tree.rebuild_separator_directory().unwrap();
    let after_rebuild = tree.stats().unwrap();
    assert!(after_rebuild.internal_pages > 0);
    assert_eq!(after_rebuild.allocated_pages, before.allocated_pages);

    let out = tree.range(Some(b"k1198"), Some(b"k1199"), None).unwrap();
    assert_eq!(
        out,
        vec![
            VarEntry {
                key: b"k1198".to_vec(),
                value: 1198
            },
            VarEntry {
                key: b"k1199".to_vec(),
                value: 1199
            },
        ]
    );
}
