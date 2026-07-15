use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use secrecy::SecretString;

use crate::{OpaqueWakeHint, PushEnvironment, PushProviderKind};

pub mod apns;
pub mod fcm;
mod token_source;

pub use apns::{ApnsConfig, ApnsProvider};
pub use fcm::{FcmConfig, FcmProvider};
pub use token_source::BearerTokenFile;

pub struct PushAttempt {
    pub token: SecretString,
    pub hint: OpaqueWakeHint,
}

impl fmt::Debug for PushAttempt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PushAttempt")
            .field("token", &"[redacted]")
            .field("hint", &"[opaque wake hint]")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderOutcome {
    Accepted,
    Suppressed,
    InvalidToken,
    Retry { retry_after_ms: Option<i64> },
    PermanentFailure,
}

#[async_trait]
pub trait PushProvider: Send + Sync {
    fn kind(&self) -> PushProviderKind;
    fn environment(&self) -> PushEnvironment;
    async fn send(&self, attempt: &PushAttempt, now_ms: i64) -> ProviderOutcome;
}

#[derive(Clone)]
pub struct ProviderRegistry {
    apns: Arc<dyn PushProvider>,
    apns_sandbox: Arc<dyn PushProvider>,
    fcm: Arc<dyn PushProvider>,
}

impl fmt::Debug for ProviderRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRegistry")
            .field("apns", &self.apns.kind())
            .field("apns_sandbox", &self.apns_sandbox.kind())
            .field("fcm", &self.fcm.kind())
            .finish()
    }
}

impl ProviderRegistry {
    pub fn new(
        apns: Arc<dyn PushProvider>,
        apns_sandbox: Arc<dyn PushProvider>,
        fcm: Arc<dyn PushProvider>,
    ) -> Self {
        debug_assert_eq!(apns.kind(), PushProviderKind::Apns);
        debug_assert_eq!(apns.environment(), PushEnvironment::Production);
        debug_assert_eq!(apns_sandbox.kind(), PushProviderKind::Apns);
        debug_assert_eq!(apns_sandbox.environment(), PushEnvironment::Sandbox);
        debug_assert_eq!(fcm.kind(), PushProviderKind::Fcm);
        debug_assert_eq!(fcm.environment(), PushEnvironment::Production);
        Self {
            apns,
            apns_sandbox,
            fcm,
        }
    }

    pub fn disabled() -> Self {
        Self::new(
            Arc::new(DisabledProvider::new(
                PushProviderKind::Apns,
                PushEnvironment::Production,
            )),
            Arc::new(DisabledProvider::new(
                PushProviderKind::Apns,
                PushEnvironment::Sandbox,
            )),
            Arc::new(DisabledProvider::new(
                PushProviderKind::Fcm,
                PushEnvironment::Production,
            )),
        )
    }

    pub fn provider(
        &self,
        kind: PushProviderKind,
        environment: PushEnvironment,
    ) -> Arc<dyn PushProvider> {
        match (kind, environment) {
            (PushProviderKind::Apns, PushEnvironment::Production) => Arc::clone(&self.apns),
            (PushProviderKind::Apns, PushEnvironment::Sandbox) => Arc::clone(&self.apns_sandbox),
            (PushProviderKind::Fcm, PushEnvironment::Production) => Arc::clone(&self.fcm),
            (PushProviderKind::Fcm, PushEnvironment::Sandbox) => Arc::new(DisabledProvider::new(
                PushProviderKind::Fcm,
                PushEnvironment::Sandbox,
            )),
        }
    }
}

#[derive(Debug)]
pub struct DisabledProvider {
    kind: PushProviderKind,
    environment: PushEnvironment,
}

impl DisabledProvider {
    pub fn new(kind: PushProviderKind, environment: PushEnvironment) -> Self {
        Self { kind, environment }
    }
}

#[async_trait]
impl PushProvider for DisabledProvider {
    fn kind(&self) -> PushProviderKind {
        self.kind
    }

    fn environment(&self) -> PushEnvironment {
        self.environment
    }

    async fn send(&self, _attempt: &PushAttempt, _now_ms: i64) -> ProviderOutcome {
        ProviderOutcome::Suppressed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockPushRecord {
    pub provider: PushProviderKind,
    pub hint: OpaqueWakeHint,
}

pub struct MockProvider {
    kind: PushProviderKind,
    environment: PushEnvironment,
    records: Mutex<Vec<MockPushRecord>>,
    outcomes: Mutex<VecDeque<ProviderOutcome>>,
}

impl fmt::Debug for MockProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockProvider")
            .field("kind", &self.kind)
            .field("records", &"[opaque records]")
            .finish()
    }
}

impl MockProvider {
    pub fn new(kind: PushProviderKind, environment: PushEnvironment) -> Self {
        Self {
            kind,
            environment,
            records: Mutex::new(Vec::new()),
            outcomes: Mutex::new(VecDeque::new()),
        }
    }

    pub fn script(&self, outcomes: impl IntoIterator<Item = ProviderOutcome>) {
        self.outcomes
            .lock()
            .expect("mock outcome lock")
            .extend(outcomes);
    }

    pub fn records(&self) -> Vec<MockPushRecord> {
        self.records.lock().expect("mock record lock").clone()
    }
}

#[async_trait]
impl PushProvider for MockProvider {
    fn kind(&self) -> PushProviderKind {
        self.kind
    }

    fn environment(&self) -> PushEnvironment {
        self.environment
    }

    async fn send(&self, attempt: &PushAttempt, _now_ms: i64) -> ProviderOutcome {
        self.records
            .lock()
            .expect("mock record lock")
            .push(MockPushRecord {
                provider: self.kind,
                hint: attempt.hint.clone(),
            });
        self.outcomes
            .lock()
            .expect("mock outcome lock")
            .pop_front()
            .unwrap_or(ProviderOutcome::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventClass, OpaqueId, SCHEMA_VERSION};

    fn hint() -> OpaqueWakeHint {
        OpaqueWakeHint {
            schema_version: SCHEMA_VERSION,
            installation_id: OpaqueId::parse("inst_0123456789abcdef").unwrap(),
            event_id: OpaqueId::parse("evt_0123456789abcdef0").unwrap(),
            cursor: 7,
            event_class: EventClass::StateChanged,
            expires_at_ms: 9_999,
        }
    }

    #[tokio::test]
    async fn disabled_provider_never_needs_network_or_credentials() {
        let provider = DisabledProvider::new(PushProviderKind::Apns, PushEnvironment::Sandbox);
        let outcome = provider
            .send(
                &PushAttempt {
                    token: SecretString::from("secret-token".to_owned()),
                    hint: hint(),
                },
                1,
            )
            .await;
        assert_eq!(outcome, ProviderOutcome::Suppressed);
    }

    #[tokio::test]
    async fn mock_records_no_provider_token() {
        let provider = MockProvider::new(PushProviderKind::Fcm, PushEnvironment::Production);
        provider
            .send(
                &PushAttempt {
                    token: SecretString::from("canary-provider-token".to_owned()),
                    hint: hint(),
                },
                1,
            )
            .await;
        let debug = format!("{:?}", provider.records());
        assert!(!debug.contains("canary-provider-token"));
    }
}
