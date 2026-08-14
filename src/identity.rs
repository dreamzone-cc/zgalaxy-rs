use std::fmt;
use std::str::FromStr;
use ed25519_dalek::{SigningKey, VerifyingKey, Signer, Verifier, Signature};
use sha2::{Digest, Sha512};
use rand::rngs::OsRng;
use anyhow::{bail, Result};
use serde::{Serialize, Deserialize};

/// A 40-bit ZeroTier Node Address (10 hexadecimal characters).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
pub struct Address(pub [u8; 5]);

impl Address {
    pub const NULL: Self = Address([0; 5]);

    /// Derive a 40-bit Address from a 32-byte Ed25519 / Curve25519 public key.
    /// In ZeroTier, the address is the last 5 bytes (40 bits) of the SHA-512 digest of the public key.
    pub fn from_public_key(pubkey: &[u8; 32]) -> Self {
        let mut hasher = Sha512::new();
        hasher.update(pubkey);
        let digest = hasher.finalize();
        let mut addr = [0u8; 5];
        addr.copy_from_slice(&digest[59..64]);
        Address(addr)
    }

    /// Check if the address starts with the reserved prefix (0xff).
    pub fn is_reserved(&self) -> bool {
        self.0[0] == 0xff
    }

    pub fn to_u64(&self) -> u64 {
        ((self.0[0] as u64) << 32)
            | ((self.0[1] as u64) << 24)
            | ((self.0[2] as u64) << 16)
            | ((self.0[3] as u64) << 8)
            | (self.0[4] as u64)
    }

    pub fn from_u64(val: u64) -> Self {
        Address([
            ((val >> 32) & 0xff) as u8,
            ((val >> 24) & 0xff) as u8,
            ((val >> 16) & 0xff) as u8,
            ((val >> 8) & 0xff) as u8,
            (val & 0xff) as u8,
        ])
    }

    pub fn as_bytes(&self) -> &[u8; 5] {
        &self.0
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4])
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({})", self)
    }
}

impl FromStr for Address {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let trimmed = s.trim();
        if trimmed.len() != 10 {
            bail!("Invalid ZeroTier address length: expected 10 hex chars, got {}", trimmed.len());
        }
        let bytes = hex::decode(trimmed)?;
        if bytes.len() != 5 {
            bail!("Decoded address length must be 5 bytes");
        }
        let mut arr = [0u8; 5];
        arr.copy_from_slice(&bytes);
        Ok(Address(arr))
    }
}

/// Cryptographic Identity representing a ZGALAXY / ZeroTier node.
#[derive(Clone)]
pub struct Identity {
    pub address: Address,
    pub verifying_key: VerifyingKey,
    pub signing_key: Option<SigningKey>,
}

impl Identity {
    /// Generate a new cryptographic identity with Hashcash Proof of Work.
    pub fn generate() -> Self {
        loop {
            let signing_key = SigningKey::generate(&mut OsRng);
            let verifying_key = signing_key.verifying_key();
            let pub_bytes = verifying_key.to_bytes();

            let mut hasher = Sha512::new();
            hasher.update(pub_bytes);
            let digest = hasher.finalize();

            // Hashcash difficulty condition: digest[0] < 17 and address is not reserved
            if digest[0] < 17 {
                let mut addr_bytes = [0u8; 5];
                addr_bytes.copy_from_slice(&digest[59..64]);
                let address = Address(addr_bytes);

                if !address.is_reserved() {
                    return Identity {
                        address,
                        verifying_key,
                        signing_key: Some(signing_key),
                    };
                }
            }
        }
    }

    /// Sign data using Ed25519.
    pub fn sign(&self, data: &[u8]) -> Result<[u8; 64]> {
        let signing_key = match &self.signing_key {
            Some(sk) => sk,
            None => bail!("Cannot sign data without secret identity signing key"),
        };
        let signature = signing_key.sign(data);
        Ok(signature.to_bytes())
    }

    /// Verify signature on data using Ed25519.
    pub fn verify(&self, data: &[u8], signature_bytes: &[u8; 64]) -> bool {
        let signature = match Signature::from_slice(signature_bytes) {
            Ok(s) => s,
            Err(_) => return false,
        };
        self.verifying_key.verify(data, &signature).is_ok()
    }

    /// Serialize public identity string: `<address>:0:<pubkey_hex>`
    pub fn to_public_string(&self) -> String {
        let pub_hex = hex::encode(self.verifying_key.to_bytes());
        format!("{}:0:{}", self.address, pub_hex)
    }

    /// Serialize secret identity string: `<address>:0:<pubkey_hex>:<privkey_hex>`
    pub fn to_secret_string(&self) -> Result<String> {
        let signing_key = match &self.signing_key {
            Some(sk) => sk,
            None => bail!("Secret key is not present in this identity"),
        };
        let pub_hex = hex::encode(self.verifying_key.to_bytes());
        let priv_hex = hex::encode(signing_key.to_bytes());
        Ok(format!("{}:0:{}:{}", self.address, pub_hex, priv_hex))
    }

    /// Parse a public or secret identity string.
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.trim().split(':').collect();
        if parts.len() < 3 {
            bail!("Malformed identity string format: expected '<address>:0:<pubkey>[:<privkey>]'");
        }

        let address = Address::from_str(parts[0])?;
        let pub_bytes = hex::decode(parts[2])?;
        if pub_bytes.len() != 32 {
            bail!("Public key must be 32 bytes");
        }
        let mut pub_arr = [0u8; 32];
        pub_arr.copy_from_slice(&pub_bytes);

        // Verify that the address correctly derives from the public key
        let expected_addr = Address::from_public_key(&pub_arr);
        if expected_addr != address {
            bail!("Address {} does not match derived public key address {}", address, expected_addr);
        }

        let verifying_key = VerifyingKey::from_bytes(&pub_arr)
            .map_err(|e| anyhow::anyhow!("Invalid Ed25519 verifying key: {}", e))?;

        let signing_key = if parts.len() >= 4 && !parts[3].is_empty() {
            let priv_bytes = hex::decode(parts[3])?;
            if priv_bytes.len() != 32 {
                bail!("Private key must be 32 bytes");
            }
            let mut priv_arr = [0u8; 32];
            priv_arr.copy_from_slice(&priv_bytes);
            Some(SigningKey::from_bytes(&priv_arr))
        } else {
            None
        };

        Ok(Identity {
            address,
            verifying_key,
            signing_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_generation_and_derivation() {
        let id = Identity::generate();
        assert_ne!(id.address, Address::NULL);
        assert!(!id.address.is_reserved());

        let pub_str = id.to_public_string();
        let parsed_pub = Identity::parse(&pub_str).unwrap();
        assert_eq!(parsed_pub.address, id.address);
        assert_eq!(parsed_pub.verifying_key, id.verifying_key);
        assert!(parsed_pub.signing_key.is_none());

        let sec_str = id.to_secret_string().unwrap();
        let parsed_sec = Identity::parse(&sec_str).unwrap();
        assert_eq!(parsed_sec.address, id.address);
        assert!(parsed_sec.signing_key.is_some());

        // Test signature verification
        let msg = b"ZGALAXY Sovereign Network Mesh Authentication";
        let sig = id.sign(msg).unwrap();
        assert!(parsed_pub.verify(msg, &sig));
        assert!(!parsed_pub.verify(b"Corrupt Message", &sig));
    }
}
