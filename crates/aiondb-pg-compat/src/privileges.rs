//! Mapping + matching helpers between the parser's privilege/grant AST and
//! the catalog's privilege descriptors. Pure: each function is a total
//! function of its inputs.

use aiondb_catalog::{CatalogPrivilege, PrivilegeTarget, QualifiedName};

pub fn parser_privilege_to_catalog(privilege: aiondb_parser::Privilege) -> CatalogPrivilege {
    match privilege {
        aiondb_parser::Privilege::Select => CatalogPrivilege::Select,
        aiondb_parser::Privilege::Insert => CatalogPrivilege::Insert,
        aiondb_parser::Privilege::Update => CatalogPrivilege::Update,
        aiondb_parser::Privilege::Delete => CatalogPrivilege::Delete,
        aiondb_parser::Privilege::All => CatalogPrivilege::All,
        aiondb_parser::Privilege::Create => CatalogPrivilege::Create,
        aiondb_parser::Privilege::Usage => CatalogPrivilege::Usage,
        aiondb_parser::Privilege::Execute => CatalogPrivilege::Execute,
        aiondb_parser::Privilege::Trigger => CatalogPrivilege::Trigger,
        aiondb_parser::Privilege::References => CatalogPrivilege::References,
        aiondb_parser::Privilege::Connect => CatalogPrivilege::Connect,
        aiondb_parser::Privilege::Temporary => CatalogPrivilege::Temporary,
        aiondb_parser::Privilege::Truncate => CatalogPrivilege::Truncate,
    }
}

pub fn parser_object_name_matches_qualified_name(
    object_name: &aiondb_parser::ObjectName,
    qualified_name: &QualifiedName,
    default_schema: Option<&str>,
) -> bool {
    match object_name.parts.as_slice() {
        [schema, relation] => {
            qualified_name
                .schema
                .as_deref()
                .is_some_and(|target_schema| target_schema.eq_ignore_ascii_case(schema))
                && qualified_name.name.eq_ignore_ascii_case(relation)
        }
        [relation] => {
            if !qualified_name.name.eq_ignore_ascii_case(relation) {
                return false;
            }
            match (default_schema, qualified_name.schema.as_deref()) {
                (Some(schema), Some(target_schema)) => target_schema.eq_ignore_ascii_case(schema),
                (Some(_), None) => false,
                (None, Some(_)) => true,
                (None, None) => true,
            }
        }
        _ => false,
    }
}

pub fn parser_grant_target_matches_privilege_target(
    target: &aiondb_parser::GrantTarget,
    privilege_target: &PrivilegeTarget,
    default_schema: Option<&str>,
) -> bool {
    match (target, privilege_target) {
        (
            aiondb_parser::GrantTarget::Table(object_name),
            PrivilegeTarget::Table(qualified_name),
        ) => parser_object_name_matches_qualified_name(object_name, qualified_name, default_schema),
        (
            aiondb_parser::GrantTarget::Function(function_target),
            PrivilegeTarget::Function(privilege_function_target),
        ) => {
            parser_object_name_matches_qualified_name(
                &function_target.name,
                &privilege_function_target.name,
                default_schema,
            ) && function_target
                .arg_types
                .as_ref()
                .map_or(true, |arg_types| {
                    privilege_function_target.arg_types.as_ref() == Some(arg_types)
                })
        }
        (aiondb_parser::GrantTarget::Schema(expected), PrivilegeTarget::Schema(actual)) => {
            expected.eq_ignore_ascii_case(actual)
        }
        (aiondb_parser::GrantTarget::Database(expected), PrivilegeTarget::Database(actual)) => {
            expected.eq_ignore_ascii_case(actual)
        }
        (aiondb_parser::GrantTarget::Role(expected), PrivilegeTarget::Role(actual)) => {
            expected.eq_ignore_ascii_case(actual)
        }
        _ => false,
    }
}
