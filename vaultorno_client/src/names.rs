//! The naming rule (design doc §3): names that become file names are
//! lowercase ASCII letters, digits and underscores, 1 to 32 characters.

/// Longest app, binary or locker name.
pub const MAX_NAME_LEN: usize = 32;

/// Whether `name` may be used as an app, binary or locker name.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Whether `vault` has one of the forms the daemon hands out: the 64 hex
/// digits of a key fingerprint, `pkg_<app>` for a package manifest, or
/// `test_<app>` for Milestone 1's test identity.
pub fn is_valid_vault_name(vault: &str) -> bool {
    if vault.len() == 64 {
        return vault
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    }
    for prefix in ["pkg_", "test_"] {
        if let Some(app) = vault.strip_prefix(prefix) {
            return is_valid_name(app);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(is_valid_name("api_keys"));
        assert!(is_valid_name("a"));
        assert!(is_valid_name(&"x".repeat(32)));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name(&"x".repeat(33)));
        assert!(!is_valid_name("api-keys"));
        assert!(!is_valid_name("Api_keys"));
        assert!(!is_valid_name("api.keys"));
        assert!(!is_valid_name("../etc"));
        assert!(!is_valid_name("ключ"));
    }

    #[test]
    fn vault_names() {
        assert!(is_valid_vault_name(&"ab".repeat(32)));
        assert!(!is_valid_vault_name(&"AB".repeat(32)));
        assert!(is_valid_vault_name("pkg_inforno"));
        assert!(is_valid_vault_name("test_inforno"));
        assert!(!is_valid_vault_name("test_"));
        assert!(!is_valid_vault_name("inforno"));
        assert!(!is_valid_vault_name("pkg_../x"));
    }
}
