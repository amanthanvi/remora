use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use reqwest::Url;
use serde::Deserialize;

use crate::{
    ApnsConfig, ApnsProvider, BearerTokenFile, DisabledProvider, FcmConfig, FcmProvider,
    MockProvider, PostgresRelayStore, ProviderRegistry, PushEnvironment, PushProviderKind,
    RelayBackend, RelayError, RelayMetrics, RelayStore, Result, StoreLimits, TokenCipher,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentProfile {
    Hosted,
    SelfHosted,
    LocalDevelopment,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    pub deployment_profile: DeploymentProfile,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub security: SecurityConfig,
    #[serde(default)]
    pub limits: LimitConfig,
    #[serde(default)]
    pub worker: WorkerConfig,
    #[serde(default)]
    pub push: PushConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: SocketAddr,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DatabaseConfig {
    Postgres {
        #[serde(default = "default_database_url_env")]
        url_env: String,
        #[serde(default = "default_max_connections")]
        max_connections: u32,
    },
    LocalSqlite {
        path: PathBuf,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    pub token_key_path: PathBuf,
    #[serde(default)]
    pub bootstrap_token_file: Option<PathBuf>,
    #[serde(default)]
    pub allow_unauthenticated_bootstrap_on_loopback: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitConfig {
    pub max_event_bytes: usize,
    pub max_snapshot_bytes: usize,
    pub max_event_ttl_ms: i64,
    pub max_snapshot_ttl_ms: i64,
    pub max_page_size: u32,
    pub max_push_token_bytes: usize,
    pub lease_ms: i64,
    pub retry_base_ms: i64,
    pub retry_cap_ms: i64,
    pub max_delivery_attempts: u32,
    pub installation_receipt_ttl_ms: i64,
    pub tombstone_retention_ms: i64,
}

impl Default for LimitConfig {
    fn default() -> Self {
        let limits = StoreLimits::default();
        Self {
            max_event_bytes: limits.max_event_bytes,
            max_snapshot_bytes: limits.max_snapshot_bytes,
            max_event_ttl_ms: limits.max_event_ttl_ms,
            max_snapshot_ttl_ms: limits.max_snapshot_ttl_ms,
            max_page_size: limits.max_page_size,
            max_push_token_bytes: limits.max_push_token_bytes,
            lease_ms: limits.lease_ms,
            retry_base_ms: limits.retry_base_ms,
            retry_cap_ms: limits.retry_cap_ms,
            max_delivery_attempts: limits.max_delivery_attempts,
            installation_receipt_ttl_ms: limits.installation_receipt_ttl_ms,
            tombstone_retention_ms: limits.tombstone_retention_ms,
        }
    }
}

impl From<LimitConfig> for StoreLimits {
    fn from(value: LimitConfig) -> Self {
        Self {
            max_event_bytes: value.max_event_bytes,
            max_snapshot_bytes: value.max_snapshot_bytes,
            max_event_ttl_ms: value.max_event_ttl_ms,
            max_snapshot_ttl_ms: value.max_snapshot_ttl_ms,
            max_page_size: value.max_page_size,
            max_push_token_bytes: value.max_push_token_bytes,
            lease_ms: value.lease_ms,
            retry_base_ms: value.retry_base_ms,
            retry_cap_ms: value.retry_cap_ms,
            max_delivery_attempts: value.max_delivery_attempts,
            installation_receipt_ttl_ms: value.installation_receipt_ttl_ms,
            tombstone_retention_ms: value.tombstone_retention_ms,
        }
    }
}

impl LimitConfig {
    /// Maximum JSON body after accounting for URL-safe base64 expansion and
    /// bounded structural overhead. The storage limits apply to decoded bytes.
    pub fn max_http_body_bytes(&self) -> usize {
        base64_encoded_len(self.max_event_bytes)
            .saturating_add(base64_encoded_len(self.max_snapshot_bytes))
            .saturating_add(16 * 1_024)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorkerConfig {
    pub batch_size: u32,
    pub poll_interval_ms: u64,
    pub maintenance_interval_ms: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            batch_size: 64,
            poll_interval_ms: 500,
            maintenance_interval_ms: 60_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushMode {
    Disabled,
    Mock,
    Live,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PushConfig {
    pub mode: PushMode,
    pub apns_production: Option<ApnsProviderConfig>,
    pub apns_sandbox: Option<ApnsProviderConfig>,
    pub fcm: Option<FcmProviderConfig>,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            mode: PushMode::Disabled,
            apns_production: None,
            apns_sandbox: None,
            fcm: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApnsProviderConfig {
    pub topic: String,
    pub bearer_token_file: PathBuf,
    #[serde(default = "default_provider_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FcmProviderConfig {
    pub project_id: String,
    pub bearer_token_file: PathBuf,
    #[serde(default = "default_provider_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Clone)]
pub enum BootstrapAuth {
    LoopbackOnly,
    TokenHash([u8; 32]),
}

impl std::fmt::Debug for BootstrapAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoopbackOnly => formatter.write_str("BootstrapAuth::LoopbackOnly"),
            Self::TokenHash(_) => formatter.write_str("BootstrapAuth::TokenHash([redacted])"),
        }
    }
}

impl RelayConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let metadata = std::fs::metadata(path.as_ref())?;
        if metadata.len() > 512 * 1_024 {
            return Err(RelayError::Configuration(
                "configuration file is too large".into(),
            ));
        }
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)
            .map_err(|error| RelayError::Configuration(format!("invalid TOML: {error}")))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let production_profile = matches!(
            self.deployment_profile,
            DeploymentProfile::Hosted | DeploymentProfile::SelfHosted
        );
        if production_profile && !matches!(self.database, DatabaseConfig::Postgres { .. }) {
            return Err(RelayError::Configuration(
                "hosted and production self-hosted profiles require PostgreSQL".into(),
            ));
        }
        if matches!(self.deployment_profile, DeploymentProfile::Hosted)
            && self.security.bootstrap_token_file.is_none()
        {
            return Err(RelayError::Configuration(
                "hosted profile requires a bootstrap token file".into(),
            ));
        }
        if self.security.bootstrap_token_file.is_none()
            && (!matches!(self.deployment_profile, DeploymentProfile::LocalDevelopment)
                || !self.server.bind.ip().is_loopback()
                || !self.security.allow_unauthenticated_bootstrap_on_loopback)
        {
            return Err(RelayError::Configuration(
                "unauthenticated bootstrap is allowed only on an explicit loopback local profile"
                    .into(),
            ));
        }
        if production_profile && matches!(self.push.mode, PushMode::Mock) {
            return Err(RelayError::Configuration(
                "mock push mode is restricted to local development".into(),
            ));
        }
        if self.worker.batch_size == 0
            || self.worker.batch_size > 1_000
            || self.worker.poll_interval_ms < 50
            || self.worker.maintenance_interval_ms < 1_000
        {
            return Err(RelayError::Configuration("worker bounds are unsafe".into()));
        }
        validate_provider_config(&self.push)?;
        let limits: StoreLimits = self.limits.clone().into();
        limits.validate()?;
        let longest_provider_timeout = [
            self.push
                .apns_production
                .as_ref()
                .map(|provider| provider.timeout_ms),
            self.push
                .apns_sandbox
                .as_ref()
                .map(|provider| provider.timeout_ms),
            self.push.fcm.as_ref().map(|provider| provider.timeout_ms),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0);
        if matches!(self.push.mode, PushMode::Live)
            && self.limits.lease_ms
                < i64::try_from(longest_provider_timeout)
                    .unwrap_or(i64::MAX)
                    .saturating_add(5_000)
        {
            return Err(RelayError::Configuration(
                "outbox lease must exceed provider timeout by at least five seconds".into(),
            ));
        }
        Ok(())
    }

    pub async fn build_backend(&self, metrics: Arc<RelayMetrics>) -> Result<RelayBackend> {
        let allow_key_create =
            matches!(self.deployment_profile, DeploymentProfile::LocalDevelopment);
        let cipher = TokenCipher::load_or_create(&self.security.token_key_path, allow_key_create)?;
        let limits = self.limits.clone().into();
        match &self.database {
            DatabaseConfig::Postgres {
                url_env,
                max_connections,
            } => {
                let url = std::env::var(url_env).map_err(|_| {
                    RelayError::Configuration(format!(
                        "PostgreSQL URL environment variable {url_env} is not set"
                    ))
                })?;
                Ok(RelayBackend::Postgres(Arc::new(
                    PostgresRelayStore::connect(&url, *max_connections, cipher, limits, metrics)
                        .await?,
                )))
            }
            DatabaseConfig::LocalSqlite { path } => Ok(RelayBackend::LocalSqlite(Arc::new(
                RelayStore::open(path, cipher, limits, metrics)?,
            ))),
        }
    }

    pub fn build_providers(&self) -> Result<Arc<ProviderRegistry>> {
        match self.push.mode {
            PushMode::Disabled => Ok(Arc::new(ProviderRegistry::disabled())),
            PushMode::Mock => Ok(Arc::new(ProviderRegistry::new(
                Arc::new(MockProvider::new(
                    PushProviderKind::Apns,
                    PushEnvironment::Production,
                )),
                Arc::new(MockProvider::new(
                    PushProviderKind::Apns,
                    PushEnvironment::Sandbox,
                )),
                Arc::new(MockProvider::new(
                    PushProviderKind::Fcm,
                    PushEnvironment::Production,
                )),
            ))),
            PushMode::Live => {
                let apns_production = build_apns(
                    self.push.apns_production.as_ref(),
                    PushEnvironment::Production,
                )?;
                let apns_sandbox =
                    build_apns(self.push.apns_sandbox.as_ref(), PushEnvironment::Sandbox)?;
                let fcm: Arc<dyn crate::PushProvider> = match self.push.fcm.as_ref() {
                    Some(config) => Arc::new(
                        FcmProvider::new(FcmConfig {
                            endpoint: Url::parse("https://fcm.googleapis.com")
                                .expect("fixed FCM URL"),
                            project_id: config.project_id.clone(),
                            bearer_token_file: BearerTokenFile::new(&config.bearer_token_file),
                            timeout_ms: config.timeout_ms,
                        })
                        .map_err(|_| {
                            RelayError::Configuration("failed to build FCM client".into())
                        })?,
                    ),
                    None => Arc::new(DisabledProvider::new(
                        PushProviderKind::Fcm,
                        PushEnvironment::Production,
                    )),
                };
                Ok(Arc::new(ProviderRegistry::new(
                    apns_production,
                    apns_sandbox,
                    fcm,
                )))
            }
        }
    }

    pub fn bootstrap_auth(&self) -> Result<BootstrapAuth> {
        let Some(path) = self.security.bootstrap_token_file.as_ref() else {
            return Ok(BootstrapAuth::LoopbackOnly);
        };
        let metadata = std::fs::metadata(path)?;
        if metadata.len() > 16 * 1_024 {
            return Err(RelayError::Configuration(
                "bootstrap token file is unexpectedly large".into(),
            ));
        }
        let token = std::fs::read_to_string(path)?;
        let token = token.trim();
        if token.len() < 32 || token.chars().any(char::is_whitespace) {
            return Err(RelayError::Configuration(
                "bootstrap token is invalid".into(),
            ));
        }
        Ok(BootstrapAuth::TokenHash(bootstrap_hash(token)))
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.worker.poll_interval_ms)
    }

    pub fn maintenance_interval(&self) -> Duration {
        Duration::from_millis(self.worker.maintenance_interval_ms)
    }
}

pub(crate) fn bootstrap_hash(token: &str) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"remora-relay-bootstrap-v1\0");
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

fn build_apns(
    config: Option<&ApnsProviderConfig>,
    environment: PushEnvironment,
) -> Result<Arc<dyn crate::PushProvider>> {
    let Some(config) = config else {
        return Ok(Arc::new(DisabledProvider::new(
            PushProviderKind::Apns,
            environment,
        )));
    };
    let endpoint = match environment {
        PushEnvironment::Production => "https://api.push.apple.com",
        PushEnvironment::Sandbox => "https://api.sandbox.push.apple.com",
    };
    Ok(Arc::new(
        ApnsProvider::new(ApnsConfig {
            endpoint: Url::parse(endpoint).expect("fixed APNs URL"),
            topic: config.topic.clone(),
            bearer_token_file: BearerTokenFile::new(&config.bearer_token_file),
            environment,
            timeout_ms: config.timeout_ms,
        })
        .map_err(|_| RelayError::Configuration("failed to build APNs client".into()))?,
    ))
}

fn validate_provider_config(push: &PushConfig) -> Result<()> {
    for apns in [push.apns_production.as_ref(), push.apns_sandbox.as_ref()]
        .into_iter()
        .flatten()
    {
        if apns.topic.is_empty()
            || apns.topic.len() > 255
            || !apns
                .topic
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        {
            return Err(RelayError::Configuration("invalid APNs topic".into()));
        }
        if !(1_000..=60_000).contains(&apns.timeout_ms) {
            return Err(RelayError::Configuration("invalid APNs timeout".into()));
        }
    }
    if let Some(fcm) = &push.fcm {
        if fcm.project_id.is_empty()
            || fcm.project_id.len() > 128
            || !fcm
                .project_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(RelayError::Configuration("invalid FCM project id".into()));
        }
        if !(1_000..=60_000).contains(&fcm.timeout_ms) {
            return Err(RelayError::Configuration("invalid FCM timeout".into()));
        }
    }
    if matches!(push.mode, PushMode::Live)
        && push.apns_production.is_none()
        && push.apns_sandbox.is_none()
        && push.fcm.is_none()
    {
        return Err(RelayError::Configuration(
            "live push mode requires at least one provider".into(),
        ));
    }
    Ok(())
}

fn default_database_url_env() -> String {
    "REMORA_RELAY_DATABASE_URL".into()
}

const fn default_max_connections() -> u32 {
    20
}

const fn default_provider_timeout_ms() -> u64 {
    10_000
}

const fn base64_encoded_len(decoded_bytes: usize) -> usize {
    decoded_bytes
        .saturating_add(2)
        .saturating_div(3)
        .saturating_mul(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_config() -> RelayConfig {
        RelayConfig {
            deployment_profile: DeploymentProfile::LocalDevelopment,
            server: ServerConfig {
                bind: "127.0.0.1:8787".parse().unwrap(),
            },
            database: DatabaseConfig::LocalSqlite {
                path: "relay.sqlite3".into(),
            },
            security: SecurityConfig {
                token_key_path: "token.key".into(),
                bootstrap_token_file: None,
                allow_unauthenticated_bootstrap_on_loopback: true,
            },
            limits: LimitConfig::default(),
            worker: WorkerConfig::default(),
            push: PushConfig::default(),
        }
    }

    #[test]
    fn production_profiles_reject_sqlite() {
        let mut config = local_config();
        config.deployment_profile = DeploymentProfile::Hosted;
        assert!(config.validate().is_err());
    }

    #[test]
    fn unauthenticated_bootstrap_is_loopback_only() {
        let mut config = local_config();
        config.server.bind = "0.0.0.0:8787".parse().unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn production_profiles_always_require_bootstrap_authentication() {
        let mut config = local_config();
        config.deployment_profile = DeploymentProfile::SelfHosted;
        config.database = DatabaseConfig::Postgres {
            url_env: default_database_url_env(),
            max_connections: 5,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn http_limit_accounts_for_base64_expansion() {
        let limits = LimitConfig::default();
        assert!(
            limits.max_http_body_bytes()
                > limits
                    .max_event_bytes
                    .saturating_add(limits.max_snapshot_bytes)
        );
        assert_eq!(base64_encoded_len(16), 24);
    }

    #[test]
    fn mock_push_is_local_only() {
        let mut config = local_config();
        config.deployment_profile = DeploymentProfile::SelfHosted;
        config.database = DatabaseConfig::Postgres {
            url_env: default_database_url_env(),
            max_connections: 5,
        };
        config.push.mode = PushMode::Mock;
        assert!(config.validate().is_err());
    }

    #[test]
    fn live_provider_timeout_must_fit_inside_lease() {
        let mut config = local_config();
        config.push.mode = PushMode::Live;
        config.push.fcm = Some(FcmProviderConfig {
            project_id: "remora-test".into(),
            bearer_token_file: "missing-but-not-read-during-validation".into(),
            timeout_ms: 60_000,
        });
        assert!(config.validate().is_err());
        config.limits.lease_ms = 65_000;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn installation_receipt_recovery_window_is_bounded() {
        let mut config = local_config();
        config.limits.installation_receipt_ttl_ms = 0;
        assert!(config.validate().is_err());
        config.limits.installation_receipt_ttl_ms = 366 * 24 * 60 * 60 * 1_000;
        assert!(config.validate().is_err());
        config.limits.installation_receipt_ttl_ms = 365 * 24 * 60 * 60 * 1_000;
        assert!(config.validate().is_ok());
    }
}
