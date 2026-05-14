/// Return the environment value for `key`, or `default` when missing.
pub fn env_string(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

/// Return a trimmed non-empty environment value for `key`.
pub fn env_optional_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Parse an environment value as `u16`, or return `default` on parse/missing.
pub fn env_u16(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

/// Parse an environment value as bool (`true`/`1`), or return `default`.
pub fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
        .unwrap_or(default)
}
