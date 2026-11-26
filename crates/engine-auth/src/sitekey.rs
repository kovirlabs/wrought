//! The site key: encrypts TOTP secrets at rest.
//!
//! Two-factor secrets are as sensitive as passwords but, unlike passwords,
//! cannot be hashed: the server needs them to check codes. They are stored
//! encrypted with ChaCha20-Poly1305 under a key that is *not* in the
//! database, so a copy of the database (a backup, a dump, an SQL injection)
//! does not give away anyone's second factor.
//!
//! The key comes from the environment, `ENGINE_SECRET_KEY` (at least 32
//! characters, e.g. `openssl rand -base64 48`), or from a file named by
//! `[auth] secret_key_file` (a Docker or systemd secret). The text is turned
//! into the cipher key with HMAC-SHA256 under a fixed label, so any long
//! random string works.
//!
//! Stored form: `v1:` + base64(nonce ‖ ciphertext ‖ tag), with the account
//! id as associated data, so a secret copied onto another account's row
//! does not decrypt. Secrets stored before v0.4 (plain base32) are still
//! read, and are encrypted when `engine serve` starts with a key.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::secret;

/// Environment variable holding the site key.
pub const KEY_ENV: &str = "ENGINE_SECRET_KEY";

const PREFIX: &str = "v1:";

/// The key, derived; never printed.
#[derive(Clone)]
pub struct SiteKey {
    key: [u8; 32],
}

impl std::fmt::Debug for SiteKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SiteKey(***)")
    }
}

impl SiteKey {
    /// Derive the key from its text form (at least 32 characters).
    pub fn from_text(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.chars().count() < 32 {
            bail!(
                "the site key must be at least 32 characters (make one with `openssl rand -base64 48`)"
            );
        }
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(text.as_bytes()).expect("any key length");
        mac.update(b"wrought site key v1: totp secrets");
        let mut key = [0u8; 32];
        key.copy_from_slice(&mac.finalize().into_bytes());
        Ok(SiteKey { key })
    }

    /// `ENGINE_SECRET_KEY`, else the file `[auth] secret_key_file`, else
    /// `None`.
    pub fn load(file: Option<&str>) -> Result<Option<Self>> {
        if let Ok(v) = std::env::var(KEY_ENV)
            && !v.trim().is_empty()
        {
            return Self::from_text(&v).map(Some).context(KEY_ENV);
        }
        if let Some(path) = file {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("[auth] secret_key_file: reading {path}"))?;
            return Self::from_text(&text)
                .map(Some)
                .with_context(|| format!("[auth] secret_key_file {path}"));
        }
        Ok(None)
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(Key::from_slice(&self.key))
    }

    /// Encrypt a TOTP secret for account `user_id`.
    pub fn seal(&self, user_id: i64, plain: &str) -> Result<String> {
        let nonce = secret::random_bytes(12);
        let aad = aad(user_id);
        let ct = self
            .cipher()
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plain.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("encrypting a TOTP secret failed"))?;
        let mut out = nonce;
        out.extend_from_slice(&ct);
        Ok(format!("{PREFIX}{}", STANDARD_NO_PAD.encode(out)))
    }

    /// Decrypt what [`Self::seal`] made for the same account.
    pub fn open(&self, user_id: i64, stored: &str) -> Result<String> {
        let raw = stored
            .strip_prefix(PREFIX)
            .context("not an encrypted secret")?;
        let bytes = STANDARD_NO_PAD
            .decode(raw)
            .context("encrypted secret is not base64")?;
        if bytes.len() < 12 + 16 {
            bail!("encrypted secret is too short");
        }
        let (nonce, ct) = bytes.split_at(12);
        let aad = aad(user_id);
        let plain = self
            .cipher()
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad: &aad })
            .map_err(|_| {
                anyhow::anyhow!(
                    "a TOTP secret does not decrypt with this site key (was the key changed?)"
                )
            })?;
        String::from_utf8(plain).context("decrypted secret is not text")
    }
}

/// Whether a stored value is encrypted (as opposed to a pre-v0.4 plain
/// base32 secret).
pub fn is_sealed(stored: &str) -> bool {
    stored.starts_with(PREFIX)
}

fn aad(user_id: i64) -> Vec<u8> {
    format!("engine_users.totp_secret:{user_id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_and_tamper() {
        let k = SiteKey::from_text("0123456789abcdef0123456789abcdef-test").unwrap();
        let s = k.seal(7, "JBSWY3DPEHPK3PXP").unwrap();
        assert!(is_sealed(&s) && !s.contains("JBSWY3DP"));
        assert_ne!(s, k.seal(7, "JBSWY3DPEHPK3PXP").unwrap(), "random nonce");
        assert_eq!(k.open(7, &s).unwrap(), "JBSWY3DPEHPK3PXP");
        assert!(k.open(8, &s).is_err(), "bound to the account");
        let other = SiteKey::from_text("another key that is long enough!!!!").unwrap();
        assert!(other.open(7, &s).is_err());
        // Flip one character in the middle of the ciphertext.
        let mut bad: Vec<char> = s.chars().collect();
        let i = bad.len() / 2;
        bad[i] = if bad[i] == 'A' { 'B' } else { 'A' };
        let bad: String = bad.into_iter().collect();
        assert!(k.open(7, &bad).is_err());
        assert!(SiteKey::from_text("short").is_err());
        assert!(!format!("{k:?}").contains("0123"));
    }
}
