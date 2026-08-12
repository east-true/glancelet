use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rng, Rng};
use sha2::{Digest, Sha256};

pub(crate) struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

impl PkcePair {
    pub fn generate() -> Self {
        let verifier = random_urlsafe(64);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

pub(crate) fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}
