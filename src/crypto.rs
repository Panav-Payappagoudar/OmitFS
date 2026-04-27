/// AES-256-GCM encryption for chunk text stored in LanceDB.
///
/// A random 256-bit key is generated on first run and stored at
/// `~/.omitfs_data/encryption.key`. Keep this file safe — losing it means
/// losing access to all indexed content (the raw files are unaffected).
///
/// Encryption is opt-in via `encryption_enabled = true` in config.toml.

use anyhow::{Context, Result};
use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use std::path::Path;

const KEY_FILE:    &str  = "encryption.key";
const NONCE_BYTES: usize = 12; // 96-bit nonce for AES-GCM

pub struct Encryptor {
    cipher: Aes256Gcm,
}

impl Encryptor {
    /// Load existing key from disk, or generate and persist a new one.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        let key_path = data_dir.join(KEY_FILE);

        let key_bytes: Vec<u8> = if key_path.exists() {
            std::fs::read(&key_path).context("Failed to read encryption.key")?
        } else {
            // Generate a fresh random 256-bit key
            let key = Aes256Gcm::generate_key(OsRng);
            std::fs::write(&key_path, key.as_slice())
                .context("Failed to write encryption.key")?;

            // Restrict file permissions on Unix (0600)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                    .context("Failed to set encryption.key permissions")?;
            }
            key.to_vec()
        };

        if key_bytes.len() != 32 {
            anyhow::bail!("encryption.key must be exactly 32 bytes — delete it to regenerate");
        }

        let key    = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(key);
        Ok(Self { cipher })
    }

    /// Encrypt plaintext → base64(nonce || ciphertext).
    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let nonce      = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self.cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("AES-GCM encrypt failed: {e}"))?;

        let mut combined = nonce.to_vec();
        combined.extend_from_slice(&ciphertext);
        Ok(base64_encode(&combined))
    }

    /// Decrypt base64(nonce || ciphertext) → plaintext.
    pub fn decrypt(&self, encoded: &str) -> Result<String> {
        let combined = base64_decode(encoded).context("Invalid base64 in encrypted chunk")?;
        if combined.len() < NONCE_BYTES {
            anyhow::bail!("Encrypted data too short to contain nonce");
        }
        let nonce      = Nonce::from_slice(&combined[..NONCE_BYTES]);
        let ciphertext = &combined[NONCE_BYTES..];
        let plaintext  = self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("AES-GCM decrypt failed: {e}"))?;

        String::from_utf8(plaintext).context("Decrypted chunk is not valid UTF-8")
    }
}

// ─── Minimal base64 (avoids adding another heavy dep) ────────────────────────

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(B64[(b0 >> 2) as usize] as char);
        out.push(B64[((b0 & 3) << 4 | b1 >> 4) as usize] as char);
        out.push(if chunk.len() > 1 { B64[((b1 & 0xf) << 2 | b2 >> 6) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[(b2 & 0x3f) as usize] as char } else { '=' });
    }
    out
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    fn v(c: u8) -> Result<u8> {
        Ok(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+'        => 62,
            b'/'        => 63,
            b'='        => 0,
            _           => anyhow::bail!("Invalid base64 char: {c}"),
        })
    }
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'\n' && b != b'\r').collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 4 { break; }
        let (v0, v1, v2, v3) = (v(chunk[0])?, v(chunk[1])?, v(chunk[2])?, v(chunk[3])?);
        out.push(v0 << 2 | v1 >> 4);
        if chunk[2] != b'=' { out.push((v1 & 0xf) << 4 | v2 >> 2); }
        if chunk[3] != b'=' { out.push((v2 & 3) << 6 | v3); }
    }
    Ok(out)
}
