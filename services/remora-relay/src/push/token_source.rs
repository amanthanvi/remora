use std::{
    fmt,
    path::{Path, PathBuf},
};

use secrecy::SecretString;

use crate::{RelayError, Result};

#[derive(Clone)]
pub struct BearerTokenFile {
    path: PathBuf,
}

impl fmt::Debug for BearerTokenFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerTokenFile([path redacted])")
    }
}

impl BearerTokenFile {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Read a short-lived provider bearer token provisioned by a credential
    /// agent. The relay never logs or returns the token.
    pub async fn load(&self) -> Result<SecretString> {
        let metadata = tokio::fs::metadata(&self.path).await?;
        if metadata.len() > 16 * 1_024 {
            return Err(RelayError::Configuration(
                "provider bearer token file is unexpectedly large".into(),
            ));
        }
        let token = tokio::fs::read_to_string(&self.path).await?;
        let token = token.trim().to_owned();
        if token.len() < 32 || token.chars().any(char::is_whitespace) {
            return Err(RelayError::Configuration(
                "provider bearer token file is invalid".into(),
            ));
        }
        Ok(SecretString::from(token))
    }
}
