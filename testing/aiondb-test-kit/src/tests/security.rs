use super::*;

// =======================================================================
// 11. Security dual-mode tests
// =======================================================================

#[tokio::test]
async fn security_cleartext_password_auth_succeeds_in_both_modes() -> DbResult<()> {
    let scenario = SqlScenario::new("security_cleartext_success", "SELECT 1")
        .with_user("alice")
        .with_cleartext_password("s3cret");
    assert_scenario_matches_with_password_auth(&scenario).await
}

#[tokio::test]
async fn security_cleartext_password_auth_rejects_wrong_password_in_both_modes() -> DbResult<()> {
    let scenario = SqlScenario::new("security_cleartext_failure", "SELECT 1")
        .with_user("alice")
        .with_cleartext_password("wrong-password")
        .expect_error();
    assert_scenario_matches_with_password_auth(&scenario).await
}

#[tokio::test]
async fn security_cleartext_password_auth_rejects_unknown_user_in_both_modes() -> DbResult<()> {
    let scenario = SqlScenario::new("security_unknown_user", "SELECT 1")
        .with_user("unknown_user")
        .with_cleartext_password("s3cret")
        .expect_error();
    assert_scenario_matches_with_password_auth(&scenario).await
}

#[tokio::test]
async fn security_cleartext_password_auth_allows_sql_after_login() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "security_sql_after_auth",
        "CREATE TABLE sec_test (id INT); \
             INSERT INTO sec_test VALUES (1), (2); \
             SELECT id FROM sec_test ORDER BY id",
    )
    .with_user("alice")
    .with_cleartext_password("s3cret");
    assert_scenario_matches_with_password_auth(&scenario).await
}

#[tokio::test]
async fn security_cleartext_password_auth_rejects_empty_password() -> DbResult<()> {
    let scenario = SqlScenario::new("security_empty_password", "SELECT 1")
        .with_user("alice")
        .with_cleartext_password("")
        .expect_error();
    assert_scenario_matches_with_password_auth(&scenario).await
}
