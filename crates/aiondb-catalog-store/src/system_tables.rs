pub const DEFAULT_SCHEMA_NAME: &str = aiondb_catalog::pg_catalog::PUBLIC_SCHEMA_NAME;

pub const SYSTEM_SCHEMA_NAMES: &[&str] = aiondb_catalog::pg_catalog::BUILTIN_SCHEMA_NAMES;

#[cfg(test)]
mod tests {
    use super::*;

    // ===================================================================
    // DEFAULT_SCHEMA_NAME
    // ===================================================================

    #[test]
    fn default_schema_name_is_public() {
        assert_eq!(DEFAULT_SCHEMA_NAME, "public");
    }

    #[test]
    fn default_schema_name_is_not_empty() {
        assert!(!DEFAULT_SCHEMA_NAME.is_empty());
    }

    #[test]
    fn default_schema_name_is_lowercase() {
        assert_eq!(DEFAULT_SCHEMA_NAME, DEFAULT_SCHEMA_NAME.to_lowercase());
    }

    // ===================================================================
    // SYSTEM_SCHEMA_NAMES
    // ===================================================================

    #[test]
    fn system_schema_names_is_not_empty() {
        assert!(!SYSTEM_SCHEMA_NAMES.is_empty());
    }

    #[test]
    fn system_schema_names_contains_public() {
        assert!(
            SYSTEM_SCHEMA_NAMES.contains(&"public"),
            "SYSTEM_SCHEMA_NAMES must include 'public'"
        );
    }

    #[test]
    fn system_schema_names_entries_are_not_empty_strings() {
        for name in SYSTEM_SCHEMA_NAMES {
            assert!(!name.is_empty(), "schema name should not be empty");
        }
    }

    #[test]
    fn system_schema_names_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for name in SYSTEM_SCHEMA_NAMES {
            assert!(seen.insert(*name), "duplicate schema name: {name}");
        }
    }

    #[test]
    fn default_schema_name_is_in_system_schema_names() {
        assert!(
            SYSTEM_SCHEMA_NAMES.contains(&DEFAULT_SCHEMA_NAME),
            "DEFAULT_SCHEMA_NAME must be one of SYSTEM_SCHEMA_NAMES"
        );
    }

    #[test]
    fn system_schema_names_matches_builtin_schema_names() {
        assert_eq!(
            SYSTEM_SCHEMA_NAMES,
            aiondb_catalog::pg_catalog::BUILTIN_SCHEMA_NAMES
        );
    }

    #[test]
    fn default_schema_name_matches_public_schema_name() {
        assert_eq!(
            DEFAULT_SCHEMA_NAME,
            aiondb_catalog::pg_catalog::PUBLIC_SCHEMA_NAME
        );
    }
}
