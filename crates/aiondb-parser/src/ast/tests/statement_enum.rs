use super::*;

// ===================================================================
// Statement enum construction and span() -- every variant
// ===================================================================

#[test]
fn statement_begin_no_mode_span() {
    let stmt = Statement::Begin {
        mode: None,
        read_only: None,
        deferrable: None,
        span: s(0, 5),
    };
    assert_eq!(stmt.span(), s(0, 5));
}

#[test]
fn statement_begin_with_read_committed_span() {
    let stmt = Statement::Begin {
        mode: Some(TransactionMode::ReadCommitted),
        read_only: None,
        deferrable: None,
        span: s(0, 38),
    };
    assert_eq!(stmt.span(), s(0, 38));
}

#[test]
fn statement_begin_with_snapshot_isolation_span() {
    let stmt = Statement::Begin {
        mode: Some(TransactionMode::SnapshotIsolation),
        read_only: None,
        deferrable: None,
        span: s(0, 42),
    };
    assert_eq!(stmt.span(), s(0, 42));
}

#[test]
fn statement_commit_span() {
    let stmt = Statement::Commit { span: s(0, 6) };
    assert_eq!(stmt.span(), s(0, 6));
}

#[test]
fn statement_rollback_span() {
    let stmt = Statement::Rollback { span: s(0, 8) };
    assert_eq!(stmt.span(), s(0, 8));
}

#[test]
fn statement_create_table_span() {
    let ct = CreateTableStatement {
        name: obj(&["t"], s(13, 14)),
        columns: vec![],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 16),
    };
    let stmt = Statement::CreateTable(ct);
    assert_eq!(stmt.span(), s(0, 16));
}

#[test]
fn statement_create_index_span() {
    let ci = CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 23),
    };
    let stmt = Statement::CreateIndex(ci);
    assert_eq!(stmt.span(), s(0, 23));
}

#[test]
fn statement_create_sequence_span() {
    let cs = CreateSequenceStatement {
        name: obj(&["seq"], s(16, 19)),
        if_not_exists: false,
        span: s(0, 19),
    };
    let stmt = Statement::CreateSequence(cs);
    assert_eq!(stmt.span(), s(0, 19));
}

#[test]
fn statement_drop_table_span() {
    let dt = DropTableStatement {
        name: obj(&["t"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 12),
    };
    let stmt = Statement::DropTable(dt);
    assert_eq!(stmt.span(), s(0, 12));
}

#[test]
fn statement_drop_index_span() {
    let di = DropIndexStatement {
        name: obj(&["idx"], s(11, 14)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 14),
    };
    let stmt = Statement::DropIndex(di);
    assert_eq!(stmt.span(), s(0, 14));
}

#[test]
fn statement_drop_sequence_span() {
    let ds = DropSequenceStatement {
        name: obj(&["seq"], s(14, 17)),
        if_exists: false,
        span: s(0, 17),
    };
    let stmt = Statement::DropSequence(ds);
    assert_eq!(stmt.span(), s(0, 17));
}

#[test]
fn statement_delete_span() {
    let del = DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 13),
    };
    let stmt = Statement::Delete(del);
    assert_eq!(stmt.span(), s(0, 13));
}

#[test]
fn statement_insert_span() {
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 13),
    };
    let stmt = Statement::Insert(ins);
    assert_eq!(stmt.span(), s(0, 13));
}

#[test]
fn statement_select_span() {
    let sel = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 8),
    };
    let stmt = Statement::Select(sel);
    assert_eq!(stmt.span(), s(0, 8));
}

#[test]
fn statement_update_span() {
    let upd = UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![],
        from_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 12),
    };
    let stmt = Statement::Update(upd);
    assert_eq!(stmt.span(), s(0, 12));
}

// ===================================================================
// Statement equality, clone, debug -- every variant
// ===================================================================

#[test]
fn statement_begin_equality() {
    let a = Statement::Begin {
        mode: None,
        read_only: None,
        deferrable: None,
        span: s(0, 5),
    };
    let b = Statement::Begin {
        mode: None,
        read_only: None,
        deferrable: None,
        span: s(0, 5),
    };
    assert_eq!(a, b);
}

#[test]
fn statement_begin_inequality_different_mode() {
    let a = Statement::Begin {
        mode: None,
        read_only: None,
        deferrable: None,
        span: s(0, 5),
    };
    let b = Statement::Begin {
        mode: Some(TransactionMode::ReadCommitted),
        read_only: None,
        deferrable: None,
        span: s(0, 5),
    };
    assert_ne!(a, b);
}

#[test]
fn statement_commit_equality() {
    assert_eq!(
        Statement::Commit { span: s(0, 6) },
        Statement::Commit { span: s(0, 6) }
    );
}

#[test]
fn statement_rollback_equality() {
    assert_eq!(
        Statement::Rollback { span: s(0, 8) },
        Statement::Rollback { span: s(0, 8) }
    );
}

#[test]
fn statement_cross_variant_inequality_begin_vs_commit() {
    assert_ne!(
        Statement::Begin {
            mode: None,
            read_only: None,
            deferrable: None,
            span: s(0, 5)
        },
        Statement::Commit { span: s(0, 5) }
    );
}

#[test]
fn statement_cross_variant_inequality_commit_vs_rollback() {
    assert_ne!(
        Statement::Commit { span: s(0, 6) },
        Statement::Rollback { span: s(0, 6) }
    );
}

#[test]
fn statement_cross_variant_inequality_create_table_vs_drop_table() {
    let ct = Statement::CreateTable(CreateTableStatement {
        name: obj(&["t"], s(13, 14)),
        columns: vec![],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 16),
    });
    let dt = Statement::DropTable(DropTableStatement {
        name: obj(&["t"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 12),
    });
    assert_ne!(ct, dt);
}

#[test]
fn statement_cross_variant_inequality_create_index_vs_drop_index() {
    let ci = Statement::CreateIndex(CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 23),
    });
    let di = Statement::DropIndex(DropIndexStatement {
        name: obj(&["idx"], s(11, 14)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 14),
    });
    assert_ne!(ci, di);
}

#[test]
fn statement_cross_variant_inequality_select_vs_insert() {
    let sel = Statement::Select(SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 6),
    });
    let ins = Statement::Insert(InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 13),
    });
    assert_ne!(sel, ins);
}

#[test]
fn statement_clone_begin() {
    let stmt = Statement::Begin {
        mode: Some(TransactionMode::SnapshotIsolation),
        read_only: None,
        deferrable: None,
        span: s(0, 42),
    };
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_commit() {
    let stmt = Statement::Commit { span: s(0, 6) };
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_rollback() {
    let stmt = Statement::Rollback { span: s(0, 8) };
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_create_table() {
    let stmt = Statement::CreateTable(CreateTableStatement {
        name: obj(&["t"], s(13, 14)),
        columns: vec![ColumnDef {
            name: "id".into(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
            default: None,
            primary_key: false,
            unique: false,
            identity: None,
            raw_type_name: None,
            inline_references: Vec::new(),
            inline_checks: Vec::new(),
            span: s(16, 22),
        }],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 23),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_create_index() {
    let stmt = Statement::CreateIndex(CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![obj(&["id"], s(23, 25))],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 26),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_create_sequence() {
    let stmt = Statement::CreateSequence(CreateSequenceStatement {
        name: obj(&["seq"], s(16, 19)),
        if_not_exists: false,
        span: s(0, 19),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_drop_table() {
    let stmt = Statement::DropTable(DropTableStatement {
        name: obj(&["t"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 12),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_drop_index() {
    let stmt = Statement::DropIndex(DropIndexStatement {
        name: obj(&["idx"], s(11, 14)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 14),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_drop_sequence() {
    let stmt = Statement::DropSequence(DropSequenceStatement {
        name: obj(&["seq"], s(14, 17)),
        if_exists: false,
        span: s(0, 17),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_delete() {
    let stmt = Statement::Delete(DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: Some(lit_bool(true, s(20, 24))),
        where_span: Some(s(14, 24)),
        returning: vec![],
        span: s(0, 24),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_insert() {
    let stmt = Statement::Insert(InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![vec![lit_int(1, s(22, 23))]],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 24),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_select() {
    let stmt = Statement::Select(SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: lit_int(1, s(7, 8)),
            alias: None,
            span: s(7, 8),
        }],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 8),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_clone_update() {
    let stmt = Statement::Update(UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![UpdateAssignment {
            column: "x".into(),
            expr: lit_int(1, s(15, 16)),
            span: s(13, 16),
        }],
        from_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 16),
    });
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn statement_debug_begin() {
    let stmt = Statement::Begin {
        mode: None,
        read_only: None,
        deferrable: None,
        span: s(0, 5),
    };
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("Begin"));
}

#[test]
fn statement_debug_commit() {
    let stmt = Statement::Commit { span: s(0, 6) };
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("Commit"));
}

#[test]
fn statement_debug_rollback() {
    let stmt = Statement::Rollback { span: s(0, 8) };
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("Rollback"));
}

#[test]
fn statement_debug_create_table() {
    let stmt = Statement::CreateTable(CreateTableStatement {
        name: obj(&["t"], s(13, 14)),
        columns: vec![],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 16),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("CreateTable"));
}

#[test]
fn statement_debug_create_index() {
    let stmt = Statement::CreateIndex(CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 23),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("CreateIndex"));
}

#[test]
fn statement_debug_create_sequence() {
    let stmt = Statement::CreateSequence(CreateSequenceStatement {
        name: obj(&["seq"], s(16, 19)),
        if_not_exists: false,
        span: s(0, 19),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("CreateSequence"));
}

#[test]
fn statement_debug_drop_table() {
    let stmt = Statement::DropTable(DropTableStatement {
        name: obj(&["t"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 12),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("DropTable"));
}

#[test]
fn statement_debug_drop_index() {
    let stmt = Statement::DropIndex(DropIndexStatement {
        name: obj(&["idx"], s(11, 14)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 14),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("DropIndex"));
}

#[test]
fn statement_debug_drop_sequence() {
    let stmt = Statement::DropSequence(DropSequenceStatement {
        name: obj(&["seq"], s(14, 17)),
        if_exists: false,
        span: s(0, 17),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("DropSequence"));
}

#[test]
fn statement_debug_delete() {
    let stmt = Statement::Delete(DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 13),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("Delete"));
}

#[test]
fn statement_debug_insert() {
    let stmt = Statement::Insert(InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 13),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("Insert"));
}

#[test]
fn statement_debug_select() {
    let stmt = Statement::Select(SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 6),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("Select"));
}

#[test]
fn statement_debug_update() {
    let stmt = Statement::Update(UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![],
        from_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 12),
    });
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("Update"));
}
