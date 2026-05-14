use crate::AuthenticatedIdentity;

pub trait IdentityExt {
    fn has_role(&self, role: &str) -> bool;
}

impl IdentityExt for AuthenticatedIdentity {
    fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|candidate| candidate == role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::DatabaseId;

    fn make_identity(roles: Vec<&str>) -> AuthenticatedIdentity {
        AuthenticatedIdentity {
            user: "testuser".to_string(),
            database_id: DatabaseId::new(1),
            roles: roles.into_iter().map(String::from).collect(),
        }
    }

    // --- has_role returns true when role is present ---
    #[test]
    fn has_role_returns_true_when_present() {
        let id = make_identity(vec!["admin", "reader"]);
        assert!(id.has_role("admin"));
        assert!(id.has_role("reader"));
    }

    // --- has_role returns false when role is absent ---
    #[test]
    fn has_role_returns_false_when_absent() {
        let id = make_identity(vec!["admin", "reader"]);
        assert!(!id.has_role("writer"));
        assert!(!id.has_role("superuser"));
    }

    // --- has_role with empty roles list ---
    #[test]
    fn has_role_empty_roles_list() {
        let id = make_identity(vec![]);
        assert!(!id.has_role("anything"));
        assert!(!id.has_role(""));
    }

    // --- has_role is case-sensitive ---
    #[test]
    fn has_role_is_case_sensitive() {
        let id = make_identity(vec!["Admin"]);
        assert!(id.has_role("Admin"));
        assert!(!id.has_role("admin"));
        assert!(!id.has_role("ADMIN"));
        assert!(!id.has_role("aDMIN"));
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    // --- has_role with single role ---

    #[test]
    fn has_role_single_role_present() {
        let id = make_identity(vec!["only_role"]);
        assert!(id.has_role("only_role"));
    }

    #[test]
    fn has_role_single_role_absent() {
        let id = make_identity(vec!["only_role"]);
        assert!(!id.has_role("other_role"));
    }

    // --- has_role with special characters in role names ---

    #[test]
    fn has_role_with_spaces_in_name() {
        let id = make_identity(vec!["role with spaces"]);
        assert!(id.has_role("role with spaces"));
        assert!(!id.has_role("role"));
    }

    #[test]
    fn has_role_with_unicode_name() {
        let id = make_identity(vec!["管理者"]);
        assert!(id.has_role("管理者"));
        assert!(!id.has_role("管理"));
    }

    #[test]
    fn has_role_with_emoji() {
        let id = make_identity(vec!["admin🔑"]);
        assert!(id.has_role("admin🔑"));
        assert!(!id.has_role("admin"));
    }

    #[test]
    fn has_role_with_special_chars() {
        let id = make_identity(vec!["role@domain.com", "role#1", "role$var"]);
        assert!(id.has_role("role@domain.com"));
        assert!(id.has_role("role#1"));
        assert!(id.has_role("role$var"));
        assert!(!id.has_role("role@"));
    }

    #[test]
    fn has_role_with_newline_in_name() {
        let id = make_identity(vec!["role\nwith\nnewlines"]);
        assert!(id.has_role("role\nwith\nnewlines"));
        assert!(!id.has_role("role"));
    }

    // --- has_role: empty string role ---

    #[test]
    fn has_role_empty_string_role_present() {
        let id = make_identity(vec![""]);
        assert!(id.has_role(""));
    }

    #[test]
    fn has_role_empty_string_role_absent_from_nonempty_roles() {
        let id = make_identity(vec!["admin"]);
        assert!(!id.has_role(""));
    }

    // --- has_role with many roles ---

    #[test]
    fn has_role_many_roles() {
        let roles: Vec<&str> = (0..1000).map(|_| "role").collect::<Vec<_>>();
        // All the same role, so has_role should find it
        let id = make_identity(roles);
        assert!(id.has_role("role"));
    }

    #[test]
    fn has_role_finds_role_at_end() {
        let mut roles: Vec<&str> = vec!["a", "b", "c", "d", "e"];
        roles.push("target");
        let id = make_identity(roles);
        assert!(id.has_role("target"));
    }

    #[test]
    fn has_role_finds_role_at_beginning() {
        let roles = vec!["target", "a", "b", "c", "d"];
        let id = make_identity(roles);
        assert!(id.has_role("target"));
    }

    // --- has_role: duplicate roles ---

    #[test]
    fn has_role_duplicate_roles() {
        let id = make_identity(vec!["admin", "admin", "admin"]);
        assert!(id.has_role("admin"));
    }

    // --- has_role: substring does not match ---

    #[test]
    fn has_role_substring_does_not_match() {
        let id = make_identity(vec!["administrator"]);
        assert!(!id.has_role("admin"));
        assert!(!id.has_role("istrator"));
    }

    // --- has_role: very long role name ---

    #[test]
    fn has_role_very_long_role_name() {
        let long_role = "r".repeat(10_000);
        let id = AuthenticatedIdentity {
            user: "user".to_string(),
            database_id: DatabaseId::new(1),
            roles: vec![long_role.clone()],
        };
        assert!(id.has_role(&long_role));
        // Shorter substring should not match
        assert!(!id.has_role(&"r".repeat(9_999)));
    }
}
