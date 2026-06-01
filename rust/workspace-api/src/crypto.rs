/*
 * Broodlink workspace-api — secret-field encryption at rest.
 *
 * Encrypts stored credentials (email IMAP/SMTP passwords, CalDAV password)
 * with ChaCha20-Poly1305 (via `ring`, already vendored through rustls). The
 * key is a 32-byte file (0600) generated on first run, mirroring the workspace app's
 * `.app_key` approach. Ciphertext is stored as `enc:v1:<base64(nonce||ct||tag)>`;
 * values without that prefix are treated as legacy plaintext and decrypt to
 * themselves, so existing rows migrate transparently on their next write.
 */

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};

const PREFIX: &str = "enc:v1:";

pub struct Cipher {
    key_bytes: [u8; 32],
    rng: SystemRandom,
}

impl Cipher {
    /// Load the key from `path`, or generate + persist a new one (mode 0600).
    pub fn load_or_create(path: &str) -> std::io::Result<Self> {
        if let Some(dir) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(dir)?;
        }
        let key_bytes = match std::fs::read(path) {
            Ok(b) if b.len() >= 32 => {
                let mut k = [0u8; 32];
                k.copy_from_slice(&b[..32]);
                k
            }
            _ => {
                let rng = SystemRandom::new();
                let mut k = [0u8; 32];
                rng.fill(&mut k)
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "rng"))?;
                write_key_file(path, &k)?;
                k
            }
        };
        Ok(Self {
            key_bytes,
            rng: SystemRandom::new(),
        })
    }

    fn sealing_key(&self) -> LessSafeKey {
        LessSafeKey::new(
            UnboundKey::new(&CHACHA20_POLY1305, &self.key_bytes).expect("32-byte key is valid"),
        )
    }

    /// Encrypt; empty input stays empty (no row marked as "has password").
    pub fn encrypt(&self, plaintext: &str) -> String {
        if plaintext.is_empty() {
            return String::new();
        }
        let mut nonce = [0u8; NONCE_LEN];
        if self.rng.fill(&mut nonce).is_err() {
            return plaintext.to_string();
        }
        let mut in_out = plaintext.as_bytes().to_vec();
        if self
            .sealing_key()
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut in_out,
            )
            .is_err()
        {
            return plaintext.to_string();
        }
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&in_out);
        format!("{PREFIX}{}", base64::encode(blob))
    }

    /// Decrypt; legacy (unprefixed) values are returned unchanged.
    pub fn decrypt(&self, stored: &str) -> String {
        if !stored.starts_with(PREFIX) {
            return stored.to_string();
        }
        let blob = match base64::decode(&stored[PREFIX.len()..]) {
            Ok(b) if b.len() > NONCE_LEN => b,
            _ => return String::new(),
        };
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let mut narr = [0u8; NONCE_LEN];
        narr.copy_from_slice(nonce);
        let mut buf = ct.to_vec();
        match self.sealing_key().open_in_place(
            Nonce::assume_unique_for_key(narr),
            Aad::empty(),
            &mut buf,
        ) {
            Ok(pt) => String::from_utf8_lossy(pt).to_string(),
            Err(_) => String::new(),
        }
    }
}

#[cfg(unix)]
fn write_key_file(path: &str, key: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(key)
}

#[cfg(not(unix))]
fn write_key_file(path: &str, key: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_legacy() {
        let c = Cipher {
            key_bytes: [7u8; 32],
            rng: SystemRandom::new(),
        };
        let enc = c.encrypt("hunter2");
        assert!(enc.starts_with(PREFIX));
        assert_ne!(enc, "hunter2");
        assert_eq!(c.decrypt(&enc), "hunter2");
        // legacy plaintext passes through
        assert_eq!(c.decrypt("plain"), "plain");
        // empty stays empty
        assert_eq!(c.encrypt(""), "");
    }
}
