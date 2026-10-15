//! Ed25519 keys and signatures of update archives (CI signs, Flux checks). Keys travel as base64:
//! the private key as its 32-byte seed (the CI secret `FLUX_UPDATE_PRIVATE_KEY`), the public key as
//! its 32 bytes ([`crate::PUBLIC_KEY`]).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

/// A new key pair: (private seed, public key), both base64.
pub fn generate() -> Result<(String, String), String> {
    let mut seed = [0u8; 32];
    SystemRandom::new()
        .fill(&mut seed)
        .map_err(|_| "The system's random generator failed".to_string())?;
    let private = STANDARD.encode(seed);
    let public = public_key(&private)?;
    Ok((private, public))
}

/// The public key (base64) of a private seed (base64).
pub fn public_key(private_key: &str) -> Result<String, String> {
    let pair = key_pair(private_key)?;
    Ok(STANDARD.encode(pair.public_key().as_ref()))
}

/// The signature (base64) of `bytes` by a private seed (base64).
pub fn sign(bytes: &[u8], private_key: &str) -> Result<String, String> {
    let pair = key_pair(private_key)?;
    Ok(STANDARD.encode(pair.sign(bytes).as_ref()))
}

/// Checks the signature (base64) of `bytes` by a public key (base64).
pub fn verify_signature(bytes: &[u8], signature: &str, public_key: &str) -> Result<(), String> {
    let key = STANDARD
        .decode(public_key.trim())
        .ok()
        .filter(|key| key.len() == 32)
        .ok_or("The public key of updates is invalid (not base64 of 32 bytes)")?;
    let signature = STANDARD
        .decode(signature.trim())
        .ok()
        .filter(|signature| signature.len() == 64)
        .ok_or("The archive's signature is invalid (not base64 of 64 bytes)")?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(bytes, &signature)
        .map_err(|_| {
            "The archive's signature doesn't match Flux's key: it wasn't made by Flux's releases"
                .to_string()
        })
}

fn key_pair(private_key: &str) -> Result<Ed25519KeyPair, String> {
    let seed = STANDARD
        .decode(private_key.trim())
        .ok()
        .filter(|seed| seed.len() == 32)
        .ok_or("The private key is invalid (not base64 of a 32-byte seed)")?;
    Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|_| "The private key is invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let (private, public) = generate().unwrap();
        assert_eq!(public_key(&private).unwrap(), public);
        let signature = sign(b"archive", &private).unwrap();
        verify_signature(b"archive", &signature, &public).unwrap();
        assert!(verify_signature(b"archiv3", &signature, &public).is_err());
        let (_, other) = generate().unwrap();
        assert!(verify_signature(b"archive", &signature, &other).is_err());
        assert!(verify_signature(b"archive", "!!", &public).is_err());
        assert!(verify_signature(b"archive", &signature, "AAAA").is_err());
        assert!(sign(b"x", "short").is_err());
    }
}
