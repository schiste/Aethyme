//! The per-host key secret and HMAC-SHA256.
//!
//! Keys must change when a server would receive different credentials, but
//! nothing derived from a credential may be guessable from what `aethyme mcp`
//! prints or records: a plain SHA-256 of a short token or of an env file can
//! be brute-forced. Every value-derived part of a key is therefore an HMAC
//! under a random secret kept in `<host state>/mcp/key-secret` (mode 0600,
//! created on first use).

use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

pub const SECRET_FILE: &str = "key-secret";
const SECRET_BYTES: usize = 32;

/// The host's key secret, created when missing.
pub fn load_or_create(directory: &Path) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let path = directory.join(SECRET_FILE);
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() >= SECRET_BYTES => return Ok(bytes),
        Ok(_) => {
            return Err(std::io::Error::other(format!(
                "{} is shorter than {SECRET_BYTES} bytes; remove it to create a new one",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let secret = random_bytes()?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(&secret)?;
            file.sync_all()?;
            Ok(secret)
        }
        // Another process created it first: use theirs.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => std::fs::read(&path),
        Err(error) => Err(error),
    }
}

/// Random bytes from the operating system.
pub fn random_bytes() -> std::io::Result<Vec<u8>> {
    let mut bytes = vec![0u8; SECRET_BYTES];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        block_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block_key.map(|byte| byte ^ 0x36));
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block_key.map(|byte| byte ^ 0x5c));
    outer.update(inner);
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&outer.finalize());
    digest
}

/// HMAC-SHA256 as lowercase hex.
pub fn hmac_hex(key: &[u8], message: &[u8]) -> String {
    hmac_sha256(key, message)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
