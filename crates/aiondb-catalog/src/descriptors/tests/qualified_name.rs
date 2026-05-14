use super::*;

// -------------------------------------------------------------------
// QualifiedName::new with schema
// -------------------------------------------------------------------

#[test]
fn qualified_name_new_with_schema() {
    let qn = QualifiedName::new(Some("public"), "users");
    assert_eq!(qn.schema, Some("public".to_owned()));
    assert_eq!(qn.name, "users");
}

// -------------------------------------------------------------------
// QualifiedName::new without schema
// -------------------------------------------------------------------

#[test]
fn qualified_name_new_without_schema() {
    let qn = QualifiedName::new(None::<String>, "orders");
    assert_eq!(qn.schema, None);
    assert_eq!(qn.name, "orders");
}

// -------------------------------------------------------------------
// QualifiedName::qualified
// -------------------------------------------------------------------

#[test]
fn qualified_name_qualified_creates_with_schema() {
    let qn = QualifiedName::qualified("myschema", "mytable");
    assert_eq!(qn.schema, Some("myschema".to_owned()));
    assert_eq!(qn.name, "mytable");
}

// -------------------------------------------------------------------
// QualifiedName::unqualified
// -------------------------------------------------------------------

#[test]
fn qualified_name_unqualified_creates_without_schema() {
    let qn = QualifiedName::unqualified("standalone");
    assert_eq!(qn.schema, None);
    assert_eq!(qn.name, "standalone");
}

// -------------------------------------------------------------------
// QualifiedName::parse with dot
// -------------------------------------------------------------------

#[test]
fn parse_with_dot_splits_schema_and_name() {
    let qn = QualifiedName::parse("public.users");
    assert_eq!(qn.schema, Some("public".to_owned()));
    assert_eq!(qn.name, "users");
}

// -------------------------------------------------------------------
// QualifiedName::parse without dot
// -------------------------------------------------------------------

#[test]
fn parse_without_dot_no_schema() {
    let qn = QualifiedName::parse("users");
    assert_eq!(qn.schema, None);
    assert_eq!(qn.name, "users");
}

// -------------------------------------------------------------------
// QualifiedName::parse with multiple dots (only first splits)
// -------------------------------------------------------------------

#[test]
fn parse_with_multiple_dots_first_split_only() {
    let qn = QualifiedName::parse("a.b.c");
    assert_eq!(qn.schema, Some("a".to_owned()));
    assert_eq!(qn.name, "b.c");
}

// -------------------------------------------------------------------
// QualifiedName Display with schema
// -------------------------------------------------------------------

#[test]
fn display_with_schema() {
    let qn = QualifiedName::qualified("public", "users");
    assert_eq!(format!("{qn}"), "public.users");
}

// -------------------------------------------------------------------
// QualifiedName Display without schema
// -------------------------------------------------------------------

#[test]
fn display_without_schema() {
    let qn = QualifiedName::unqualified("users");
    assert_eq!(format!("{qn}"), "users");
}

// -------------------------------------------------------------------
// QualifiedName::schema_name() Some/None
// -------------------------------------------------------------------

#[test]
fn schema_name_returns_some_when_present() {
    let qn = QualifiedName::qualified("pg_catalog", "tables");
    assert_eq!(qn.schema_name(), Some("pg_catalog"));
}

#[test]
fn schema_name_returns_none_when_absent() {
    let qn = QualifiedName::unqualified("tables");
    assert_eq!(qn.schema_name(), None);
}

// -------------------------------------------------------------------
// QualifiedName::object_name()
// -------------------------------------------------------------------

#[test]
fn object_name_returns_name_with_schema() {
    let qn = QualifiedName::qualified("s", "t");
    assert_eq!(qn.object_name(), "t");
}

#[test]
fn object_name_returns_name_without_schema() {
    let qn = QualifiedName::unqualified("t");
    assert_eq!(qn.object_name(), "t");
}

// -------------------------------------------------------------------
// QualifiedName Ord (lexicographic)
// -------------------------------------------------------------------

#[test]
fn ord_none_schema_less_than_some_schema() {
    let a = QualifiedName::unqualified("z");
    let b = QualifiedName::qualified("a", "a");
    // None < Some(_) in Rust's Option Ord
    assert!(a < b);
}

#[test]
fn ord_same_schema_sorted_by_name() {
    let a = QualifiedName::qualified("s", "apple");
    let b = QualifiedName::qualified("s", "banana");
    assert!(a < b);
}

#[test]
fn ord_different_schemas_sorted_by_schema_first() {
    let a = QualifiedName::qualified("alpha", "z");
    let b = QualifiedName::qualified("beta", "a");
    assert!(a < b);
}

#[test]
fn ord_equal_elements() {
    let a = QualifiedName::qualified("s", "t");
    let b = QualifiedName::qualified("s", "t");
    assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);
}

// -------------------------------------------------------------------
// QualifiedName Hash consistency
// -------------------------------------------------------------------

#[test]
fn hash_equal_names_same_bucket() {
    let a = QualifiedName::qualified("s", "t");
    let b = QualifiedName::qualified("s", "t");
    let mut set = HashSet::new();
    set.insert(a);
    set.insert(b);
    assert_eq!(set.len(), 1);
}

#[test]
fn hash_different_names_different_buckets() {
    let a = QualifiedName::qualified("s", "t1");
    let b = QualifiedName::qualified("s", "t2");
    let mut set = HashSet::new();
    set.insert(a);
    set.insert(b);
    assert_eq!(set.len(), 2);
}

#[test]
fn hash_unqualified_vs_qualified_different() {
    let a = QualifiedName::unqualified("t");
    let b = QualifiedName::qualified("", "t");
    // schema=None vs schema=Some("") are different
    let mut set = HashSet::new();
    set.insert(a);
    set.insert(b);
    assert_eq!(set.len(), 2);
}

// -------------------------------------------------------------------
// QualifiedName::parse with empty string
// -------------------------------------------------------------------

#[test]
fn parse_empty_string() {
    let qn = QualifiedName::parse("");
    assert_eq!(qn.schema, None);
    assert_eq!(qn.name, "");
}

// -------------------------------------------------------------------
// QualifiedName::parse with dot at start
// -------------------------------------------------------------------

#[test]
fn parse_dot_at_start_gives_empty_schema() {
    let qn = QualifiedName::parse(".table");
    assert_eq!(qn.schema, Some(String::new()));
    assert_eq!(qn.name, "table");
}

// -------------------------------------------------------------------
// QualifiedName::parse with dot at end
// -------------------------------------------------------------------

#[test]
fn parse_dot_at_end_gives_empty_name() {
    let qn = QualifiedName::parse("schema.");
    assert_eq!(qn.schema, Some("schema".to_owned()));
    assert_eq!(qn.name, "");
}
