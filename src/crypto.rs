use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit},
    ChaCha20Poly1305, Nonce as ChaChaNonce, Tag,
};
use salsa20::cipher::{KeyIvInit, StreamCipher};
use salsa20::Salsa20;
use rand::rngs::OsRng;
use anyhow::{bail, Result};

/// Cryptographic engine supporting X25519, ChaCha20-Poly1305, and Salsa20.
pub struct CryptoEngine;

impl CryptoEngine {
    /// Generate an ephemeral X25519 secret and public key pair for Diffie-Hellman key exchange.
    pub fn generate_ephemeral_keypair() -> (EphemeralSecret, PublicKey) {
        let secret = EphemeralSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        (secret, public)
    }

    /// Compute shared secret between our private key and a remote public key.
    pub fn diffie_hellman(secret: EphemeralSecret, remote_public: &PublicKey) -> [u8; 32] {
        let shared = secret.diffie_hellman(remote_public);
        *shared.as_bytes()
    }

    /// Compute shared secret using a static secret key.
    pub fn diffie_hellman_static(secret: &StaticSecret, remote_public: &PublicKey) -> [u8; 32] {
        let shared = secret.diffie_hellman(remote_public);
        *shared.as_bytes()
    }

    /// Encrypt payload using ChaCha20-Poly1305 with AEAD authentication tag.
    pub fn encrypt_chacha20_poly1305(
        key: &[u8; 32],
        nonce: &[u8; 12],
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(key.into());
        let nonce_obj = ChaChaNonce::from_slice(nonce);
        let payload = chacha20poly1305::aead::Payload {
            msg: plaintext,
            aad,
        };
        let ciphertext = cipher
            .encrypt(nonce_obj, payload)
            .map_err(|e| anyhow::anyhow!("ChaCha20-Poly1305 encryption failed: {:?}", e))?;
        Ok(ciphertext)
    }

    /// Decrypt payload using ChaCha20-Poly1305 and verify authentication tag.
    pub fn decrypt_chacha20_poly1305(
        key: &[u8; 32],
        nonce: &[u8; 12],
        ciphertext_with_tag: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(key.into());
        let nonce_obj = ChaChaNonce::from_slice(nonce);
        let payload = chacha20poly1305::aead::Payload {
            msg: ciphertext_with_tag,
            aad,
        };
        let plaintext = cipher
            .decrypt(nonce_obj, payload)
            .map_err(|e| anyhow::anyhow!("ChaCha20-Poly1305 decryption / tag verification failed: {:?}", e))?;
        Ok(plaintext)
    }

    /// Salsa20 symmetric stream cipher encryption / decryption in place.
    pub fn salsa20_crypt(key: &[u8; 32], nonce: &[u8; 8], buffer: &mut [u8]) {
        let mut cipher = Salsa20::new(key.into(), nonce.into());
        cipher.apply_keystream(buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diffie_hellman_and_chacha20_poly1305() {
        let (alice_secret, alice_public) = CryptoEngine::generate_ephemeral_keypair();
        let (bob_secret, bob_public) = CryptoEngine::generate_ephemeral_keypair();

        let alice_shared = CryptoEngine::diffie_hellman(alice_secret, &bob_public);
        let bob_shared = CryptoEngine::diffie_hellman(bob_secret, &alice_public);

        assert_eq!(alice_shared, bob_shared);

        let nonce = [42u8; 12];
        let message = b"ZGALAXY Sovereign P2P Encrypted Mesh Packet";
        let aad = b"header_metadata";

        let ciphertext = CryptoEngine::encrypt_chacha20_poly1305(&alice_shared, &nonce, message, aad).unwrap();
        assert_ne!(&ciphertext[..message.len()], message);

        let decrypted = CryptoEngine::decrypt_chacha20_poly1305(&bob_shared, &nonce, &ciphertext, aad).unwrap();
        assert_eq!(decrypted, message);
    }
}
