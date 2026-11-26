//! Argon2id password hashing and the password policy.
//!
//! Parameters are the `argon2` crate's defaults: Argon2id, version 0x13,
//! 19 MiB memory, 2 iterations, 1 lane (the OWASP minimum). They are stored
//! in each hash (PHC string), so raising them later only affects new hashes.
//! Hashing takes tens of milliseconds of CPU: async callers run it with
//! `spawn_blocking`.

use std::sync::LazyLock;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// Shortest accepted password. Length is the policy; there are no
/// composition rules (NIST SP 800-63B).
pub const MIN_LEN: usize = 12;
/// Longest accepted password (bounds hashing work per request).
pub const MAX_LEN: usize = 1024;

fn argon() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default())
}

/// Hash a password for storage.
pub fn hash(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    argon()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hashing password: {e}"))
}

/// Check a password against a stored hash. Malformed hashes never match.
pub fn verify(password: &str, stored: &str) -> bool {
    if password.len() > MAX_LEN {
        return false;
    }
    match PasswordHash::new(stored) {
        Ok(h) => argon().verify_password(password.as_bytes(), &h).is_ok(),
        Err(_) => false,
    }
}

/// A real hash of a random password, verified against when an account does
/// not exist, so a failed sign-in takes the same time either way.
pub fn dummy_hash() -> &'static str {
    static H: LazyLock<String> =
        LazyLock::new(|| hash(&crate::secret::token()).expect("hashing a random password"));
    &H
}

/// A hash no password matches (accounts that must set theirs by reset).
pub fn unusable_hash() -> String {
    format!("!unusable!{}", crate::secret::token())
}

/// Why a new password is refused, if it is.
pub fn check_policy(password: &str, email: &str) -> Option<String> {
    let n = password.chars().count();
    if n < MIN_LEN {
        return Some(format!("must be at least {MIN_LEN} characters"));
    }
    if password.len() > MAX_LEN {
        return Some(format!("must be at most {MAX_LEN} bytes"));
    }
    if password.trim().is_empty()
        || password
            .chars()
            .all(|c| c == password.chars().next().unwrap())
    {
        return Some("is too easy to guess".into());
    }
    if !email.is_empty() && password.to_lowercase().contains(&email.to_lowercase()) {
        return Some("must not contain the email address".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_argon2id_and_verify() {
        let h = hash("correct horse battery staple").unwrap();
        assert!(h.starts_with("$argon2id$v=19$"), "{h}");
        assert!(verify("correct horse battery staple", &h));
        assert!(!verify("correct horse battery stapl", &h));
        assert_ne!(h, hash("correct horse battery staple").unwrap(), "salted");
        assert!(!verify("x", "not a hash"));
        assert!(!verify("x", &unusable_hash()));
        assert!(!verify("x", dummy_hash()));
    }

    #[test]
    fn policy() {
        assert!(check_policy("short", "").is_some());
        assert!(check_policy("aaaaaaaaaaaaaaaa", "").is_some());
        assert!(check_policy("ada@example.org-123", "ada@example.org").is_some());
        assert!(check_policy("a long enough passphrase", "ada@example.org").is_none());
    }
}
