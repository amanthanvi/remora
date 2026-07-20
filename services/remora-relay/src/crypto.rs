use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use zeroize::Zeroizing;

use crate::{RelayError, Result};

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;

#[derive(Clone)]
pub struct TokenCipher {
    key: Arc<Zeroizing<[u8; KEY_BYTES]>>,
}

impl std::fmt::Debug for TokenCipher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TokenCipher([redacted])")
    }
}

#[derive(Clone)]
pub(crate) struct SealedToken {
    pub nonce: [u8; NONCE_BYTES],
    pub ciphertext: Vec<u8>,
}

impl std::fmt::Debug for SealedToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SealedToken([redacted])")
    }
}

impl TokenCipher {
    pub fn from_key(key: [u8; KEY_BYTES]) -> Self {
        Self {
            key: Arc::new(Zeroizing::new(key)),
        }
    }

    /// Load a base64 key, creating a mode-0600 file when it does not exist.
    /// Hosted deployments should provision this file before startup and share
    /// it only with dispatch workers that need to decrypt provider tokens.
    pub fn load_or_create(path: &Path, allow_create: bool) -> Result<Self> {
        match Self::load(path) {
            Ok(cipher) => Ok(cipher),
            Err(RelayError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                if !allow_create {
                    return Err(RelayError::Configuration(
                        "token key file must be provisioned before hosted startup".into(),
                    ));
                }
                Self::create(path)
            }
            Err(error) => Err(error),
        }
    }

    fn load(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let metadata = fs::metadata(path)?;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(RelayError::Configuration(
                    "token key file must not be group- or world-accessible".into(),
                ));
            }
        }

        let mut encoded = Zeroizing::new(String::new());
        OpenOptions::new()
            .read(true)
            .open(path)?
            .take(4096)
            .read_to_string(&mut encoded)?;
        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(encoded.trim())
                .map_err(|_| RelayError::Configuration("token key is not valid base64".into()))?,
        );
        let key: [u8; KEY_BYTES] = decoded.as_slice().try_into().map_err(|_| {
            RelayError::Configuration("token key must decode to exactly 32 bytes".into())
        })?;
        Ok(Self::from_key(key))
    }

    fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut key = [0_u8; KEY_BYTES];
        rand::fill(&mut key);
        let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(key));

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }

        match options.open(path) {
            Ok(mut file) => {
                file.write_all(encoded.as_bytes())?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                Ok(Self::from_key(key))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Self::load(path),
            Err(error) => Err(RelayError::Io(error)),
        }
    }

    pub(crate) fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<SealedToken> {
        let mut nonce = [0_u8; NONCE_BYTES];
        rand::fill(&mut nonce);
        let cipher = XChaCha20Poly1305::new((&**self.key).into());
        let nonce_value = XNonce::try_from(nonce.as_slice()).map_err(|_| RelayError::Crypto)?;
        let ciphertext = cipher
            .encrypt(
                &nonce_value,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| RelayError::Crypto)?;
        Ok(SealedToken { nonce, ciphertext })
    }

    pub(crate) fn open(&self, sealed: &SealedToken, aad: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let cipher = XChaCha20Poly1305::new((&**self.key).into());
        let nonce_value =
            XNonce::try_from(sealed.nonce.as_slice()).map_err(|_| RelayError::Crypto)?;
        cipher
            .decrypt(
                &nonce_value,
                Payload {
                    msg: &sealed.ciphertext,
                    aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| RelayError::Crypto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip_binds_aad() {
        let cipher = TokenCipher::from_key([7; KEY_BYTES]);
        let sealed = cipher.seal(b"push-token", b"installation-a").unwrap();
        assert_eq!(
            cipher.open(&sealed, b"installation-a").unwrap().as_slice(),
            b"push-token"
        );
        assert!(cipher.open(&sealed, b"installation-b").is_err());
        assert!(!format!("{cipher:?}").contains("push-token"));
        assert!(!format!("{sealed:?}").contains("push-token"));
    }

    #[test]
    fn generated_key_file_is_reusable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.key");
        let first = TokenCipher::load_or_create(&path, true).unwrap();
        let second = TokenCipher::load_or_create(&path, false).unwrap();
        let sealed = first.seal(b"secret", b"aad").unwrap();
        assert_eq!(second.open(&sealed, b"aad").unwrap().as_slice(), b"secret");
    }
}
