//! Encrypts stored server secrets with a locally generated key file.
//!
//! This protects secrets in database backups and copies; anyone who can read
//! both the database and the key file can still decrypt them.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use chacha20poly1305::aead::{Aead, Generate, Key, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::error::{Error, Result};

const NONCE_LEN: usize = 24;

pub struct Vault {
    cipher: XChaCha20Poly1305,
}

impl Vault {
    /// Loads the key at `path`, creating it (mode 0600) if it does not exist.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            let key = Key::<XChaCha20Poly1305>::generate();
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            file.write_all(key.as_slice())?;
        }
        let bytes = fs::read(path)?;
        let cipher = XChaCha20Poly1305::new_from_slice(&bytes)
            .map_err(|_| Error::Vault(format!("{} is not a valid key file", path.display())))?;
        Ok(Vault { cipher })
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>> {
        let nonce = XNonce::generate();
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|_| Error::Vault("encryption failed".into()))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn decrypt(&self, data: &[u8]) -> Result<String> {
        if data.len() < NONCE_LEN {
            return Err(Error::Vault("stored secret is truncated".into()));
        }
        let (nonce, ciphertext) = data.split_at(NONCE_LEN);
        let nonce = XNonce::try_from(nonce).map_err(|_| Error::Vault("bad nonce".into()))?;
        let plaintext = self.cipher.decrypt(&nonce, ciphertext).map_err(|_| {
            Error::Vault("cannot decrypt stored secret (wrong or replaced key file?)".into())
        })?;
        String::from_utf8(plaintext).map_err(|_| Error::Vault("stored secret is not UTF-8".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_key_persistence() {
        let dir = std::env::temp_dir().join(format!("mgr-vault-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vault.key");
        let _ = fs::remove_file(&path);

        let sealed = Vault::open(&path).unwrap().encrypt("s3cret").unwrap();
        assert_eq!(Vault::open(&path).unwrap().decrypt(&sealed).unwrap(), "s3cret");

        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(Vault::open(&path).unwrap().decrypt(&tampered).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
