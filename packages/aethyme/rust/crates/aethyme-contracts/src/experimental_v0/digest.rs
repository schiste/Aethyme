//! The `sha256:<64 lowercase hex>` encoding shared by every v0 digest
//! identity (decision record: common encoding rules).

use data_encoding::HEXLOWER;
use sha2::{Digest, Sha256};

/// Prefix of every encoded v0 digest identity: the digest algorithm.
pub const ID_PREFIX: &str = "sha256:";

/// `sha256:` followed by the lowercase hex SHA-256 of `preimage`.
pub(crate) fn encode(preimage: &[u8]) -> String {
    format!("{ID_PREFIX}{}", HEXLOWER.encode(&Sha256::digest(preimage)))
}

/// True only for the exact canonical form: uppercase hex, other algorithms
/// and other lengths are refused rather than normalized.
pub(crate) fn is_canonical(encoded: &str) -> bool {
    encoded.strip_prefix(ID_PREFIX).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
