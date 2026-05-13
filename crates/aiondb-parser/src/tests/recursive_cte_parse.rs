use super::*;

#[test]
fn search_clause_rejects_non_top_level_recursive_reference_with_shadowed_cte_name() {
    let sql = "with recursive x(col) as (
        select 1
        union
        (with x as (select * from x)
         select * from x)
    ) search depth first by col set seq
    select * from x;";
    let err = parse_prepared_statement(sql).expect_err("expected SEARCH/CYCLE top-level error");
    let message = format!("{err}");
    assert!(
        message.contains(
            "with a SEARCH or CYCLE clause, the recursive reference to WITH query \"x\" must be at the top level of its right-hand SELECT"
        ),
        "unexpected error: {message}"
    );
}

#[test]
fn with_cte_scope_attaches_on_leftmost_setop_branch() {
    let statement = parse_prepared_statement(
        "WITH t AS (SELECT 1 AS n) SELECT n FROM t UNION ALL SELECT n FROM t",
    )
    .expect("parse");

    let Statement::SetOperation(set_op) = statement else {
        panic!("expected set operation");
    };

    let Statement::Select(left) = &*set_op.left else {
        panic!("expected left select branch");
    };
    assert!(
        left.ctes
            .iter()
            .any(|cte| cte.name.eq_ignore_ascii_case("t")),
        "left branch should carry WITH CTEs"
    );
}

#[test]
fn recursive_union_all_parsing_preserves_recursive_term_for_common_forms() {
    let samples = [
        "WITH RECURSIVE x(n) AS (SELECT 1 UNION ALL SELECT count(*) FROM x) SELECT * FROM x",
        "WITH RECURSIVE x(n) AS (SELECT 1 UNION ALL SELECT sum(n) FROM x) SELECT * FROM x",
        "WITH RECURSIVE x(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM x ORDER BY 1) SELECT * FROM x",
        "WITH RECURSIVE x(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM x LIMIT 10 OFFSET 1) SELECT * FROM x",
    ];

    for sql in samples {
        let statement = parse_prepared_statement(sql).expect("parse");
        let Statement::Select(select) = statement else {
            panic!("expected SELECT statement");
        };
        assert!(!select.ctes.is_empty(), "expected at least one CTE");
        assert!(
            select.ctes[0].recursive_term.is_some(),
            "expected recursive_term to be preserved for: {sql}"
        );
    }
}

#[test]
fn create_view_set_operation_override_sql_preserves_source_text() {
    let stmt = parse_prepared_statement(
        "CREATE RECURSIVE VIEW nums(n) AS VALUES (1) UNION ALL SELECT n + 1 FROM nums WHERE n < 5",
    )
    .expect("CREATE RECURSIVE VIEW should parse");
    let Statement::CreateView(create) = stmt else {
        panic!("expected CreateView");
    };
    let override_sql = create
        .override_sql
        .as_deref()
        .expect("set-operation view should keep override SQL");
    assert!(
        override_sql.contains("WHERE n < 5"),
        "override SQL lost predicate: {override_sql}"
    );
    assert!(
        !override_sql.contains('{'),
        "override SQL should not contain debug braces: {override_sql}"
    );
}

#[test]
fn with_recursive_shadowed_name_cte_is_not_marked_recursive() {
    let stmt = parse_prepared_statement(
        "WITH RECURSIVE w6(c6) AS (WITH w6(c6) AS (SELECT 1) SELECT * FROM w6) SELECT * FROM w6",
    )
    .expect("parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(select.ctes.len(), 1);
    assert!(
        !select.ctes[0].recursive,
        "shadowed local WITH name should not mark outer CTE recursive"
    );
    assert!(select.ctes[0].recursive_term.is_none());
}

#[test]
fn with_recursive_values_cte_without_self_reference_stays_non_recursive_term() {
    let stmt = parse_prepared_statement(
        "WITH RECURSIVE \
         graph(f, t, label) AS ( \
           VALUES (1, 2, 'arc 1 -> 2'), (1, 3, 'arc 1 -> 3') \
         ), \
         search_graph(f, t, label) AS ( \
           SELECT * FROM graph \
           UNION ALL \
           SELECT g.* FROM graph g, search_graph sg WHERE g.f = sg.t \
         ) \
         SELECT f, t, label FROM search_graph",
    )
    .expect("parse");

    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(select.ctes.len(), 2);
    assert!(
        select.ctes[0].recursive_term.is_none(),
        "VALUES-only CTE without self-reference must not be split as recursive"
    );
    assert!(
        select.ctes[1].recursive_term.is_some(),
        "self-referencing CTE should keep recursive_term"
    );
}

#[test]
fn with_recursive_parenthesized_left_arm_preserves_recursive_split() {
    let stmt = parse_prepared_statement(
        "WITH RECURSIVE x(n) AS ( \
           (WITH x1 AS (SELECT 1 FROM x) SELECT * FROM x1) \
           UNION \
           SELECT 0 \
         ) \
         SELECT * FROM x",
    )
    .expect("parse");

    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(select.ctes.len(), 1);
    assert!(select.ctes[0].recursive);
    assert!(
        select.ctes[0].recursive_term.is_some(),
        "expected parenthesized left arm UNION to preserve recursive_term split"
    );
}

#[test]
fn create_table_like_populates_inherits_sources() {
    let stmt = parse_prepared_statement("CREATE TABLE t2 (LIKE t INCLUDING ALL)").expect("parse");
    let Statement::CreateTable(create) = stmt else {
        panic!("expected CreateTable");
    };
    assert_eq!(create.inherits.len(), 1);
    assert_eq!(create.inherits[0].parts, vec!["t".to_owned()]);
}
