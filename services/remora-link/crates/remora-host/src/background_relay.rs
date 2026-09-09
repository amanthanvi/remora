//! Optional, credential-scoped wake publication. Relay data never supplies app state.

use std::{
    collections::BTreeMap,
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{anyhow, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::StreamExt;
use rand::RngCore;
use remora_bridge_core::session::SessionRegistry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

#[cfg(not(windows))]
use std::{
    io::{Read, Write},
    path::Path,
};
#[cfg(windows)]
#[path = "background_relay_windows.rs"]
mod windows;
#[cfg(windows)]
use windows::{read_private, write_private};

use crate::{
    config::BackgroundRelayConfig,
    pairing_v2::{AuthorizationContextV2, PairingManager},
};

const MAX_BODY: usize = 64 * 1024;
const MAX_JOURNAL: u64 = 4 * 1024 * 1024;
const MAX_ENROLLMENTS: usize = 128;

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelaySecret(String);

impl fmt::Debug for RelaySecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl Drop for RelaySecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayEnrollment {
    pub relay_origin: String,
    pub installation_id: String,
    pub command_id: String,
    pub read_capability: RelaySecret,
    pub manage_capability: RelaySecret,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayCommit {
    pub installation_id: String,
    pub command_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayRuntimeState {
    pub runtime_id: String,
    pub session_id: String,
    pub state_revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayBarrier {
    pub installation_id: String,
    pub through_cursor: u64,
    pub barrier_id: String,
    pub runtime_ids: Vec<String>,
    pub host_epoch: String,
    pub runtime_states: Vec<RelayRuntimeState>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Enrollment {
    credential_id: String,
    endpoint_id: String,
    auth_epoch: u64,
    runtime_ids: Vec<String>,
    scopes: Vec<crate::pairing_v2::DeviceScopeV2>,
    command_id: String,
    provisioning_key: RelaySecret,
    installation_id: Option<String>,
    write_capability: Option<RelaySecret>,
    transfer: Option<RelayEnrollment>,
    committed: bool,
    latest_cursor: u64,
    published_barrier: Option<String>,
    pending: Option<PendingEvent>,
}

impl Enrollment {
    fn authorization(&self) -> AuthorizationContextV2 {
        AuthorizationContextV2 {
            credential_id: self.credential_id.clone(),
            auth_epoch: self.auth_epoch,
            selected_runtime_ids: self.runtime_ids.clone(),
            granted_scopes: self.scopes.clone(),
        }
    }
    fn matches(&self, auth: &AuthorizationContextV2) -> bool {
        self.credential_id == auth.credential_id
            && self.auth_epoch == auth.auth_epoch
            && self.runtime_ids == auth.selected_runtime_ids
            && self.scopes == auth.granted_scopes
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingEvent {
    event_id: String,
    event_class: String,
    expires_at_ms: i64,
    ciphertext: String,
    barrier: String,
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    origin: String,
    entries: BTreeMap<String, Enrollment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IssuedInstallation {
    schema_version: u16,
    installation_id: String,
    write_capability: RelaySecret,
    read_capability: RelaySecret,
    manage_capability: RelaySecret,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Published {
    schema_version: u16,
    event_id: String,
    cursor: u64,
    replayed: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("relay request rejected")]
struct HttpStatus(u16);

pub(crate) struct BackgroundRelay {
    config: BackgroundRelayConfig,
    origin: reqwest::Url,
    client: reqwest::Client,
    path: PathBuf,
    journal: Mutex<Journal>,
    host_epoch: String,
    encryption_key: Zeroizing<[u8; 32]>,
    sessions: Arc<SessionRegistry>,
    unavailable: AtomicBool,
}

impl BackgroundRelay {
    pub fn load(
        config: BackgroundRelayConfig,
        path: PathBuf,
        sessions: Arc<SessionRegistry>,
    ) -> anyhow::Result<Arc<Self>> {
        let origin = validate_origin(&config.origin, config.allow_loopback_http)?;
        let journal: Journal = match read_private(&path, MAX_JOURNAL) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Journal {
                    origin: origin.as_str().to_owned(),
                    ..Journal::default()
                }
            }
            Err(error) => return Err(error),
        };
        ensure!(
            journal.origin == origin.as_str() && journal.entries.len() <= MAX_ENROLLMENTS,
            "relay custody origin or capacity mismatch"
        );
        for (credential, entry) in &journal.entries {
            ensure!(
                credential == &entry.credential_id
                    && !credential.is_empty()
                    && !entry.endpoint_id.is_empty()
                    && !entry.command_id.is_empty()
                    && !entry.runtime_ids.is_empty()
                    && entry.runtime_ids.len() <= 32,
                "invalid relay custody identity"
            );
            let mut runtimes = entry.runtime_ids.clone();
            runtimes.sort();
            runtimes.dedup();
            ensure!(
                runtimes.len() == entry.runtime_ids.len(),
                "duplicate relay custody runtime"
            );
            match (
                &entry.installation_id,
                &entry.write_capability,
                &entry.transfer,
                entry.committed,
            ) {
                (None, None, None, false) => ensure!(
                    entry.latest_cursor == 0
                        && entry.pending.is_none()
                        && entry.published_barrier.is_none()
                        && valid_capability(&entry.provisioning_key.0),
                    "invalid preparing relay custody"
                ),
                (Some(id), Some(write), transfer, committed) => {
                    ensure!(
                        valid_relay_id(id) && valid_capability(&write.0),
                        "invalid issued relay custody"
                    );
                    if committed {
                        ensure!(
                            transfer.is_none() && entry.provisioning_key.0.is_empty(),
                            "committed relay custody retains transfer secrets"
                        );
                    } else {
                        let transfer = transfer
                            .as_ref()
                            .ok_or_else(|| anyhow!("missing relay transfer"))?;
                        ensure!(
                            transfer.installation_id == *id
                                && transfer.command_id == entry.command_id
                                && transfer.relay_origin == origin.as_str().trim_end_matches('/')
                                && valid_capability(&transfer.read_capability.0)
                                && valid_capability(&transfer.manage_capability.0)
                                && valid_capability(&entry.provisioning_key.0),
                            "invalid relay transfer custody"
                        );
                    }
                }
                _ => return Err(anyhow!("incomplete relay custody")),
            }
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()?;
        let mut key = Zeroizing::new([0; 32]);
        rand::rngs::OsRng.fill_bytes(key.as_mut());
        Ok(Arc::new(Self {
            config,
            origin,
            client,
            path,
            journal: Mutex::new(journal),
            host_epoch: random_hex(),
            encryption_key: key,
            sessions,
            unavailable: AtomicBool::new(false),
        }))
    }

    pub async fn enroll(
        &self,
        auth: &AuthorizationContextV2,
        endpoint: &str,
        command: &str,
    ) -> anyhow::Result<RelayEnrollment> {
        let mut state = self.journal.lock().await;
        ensure!(
            !self.unavailable.load(Ordering::Acquire),
            "relay custody unavailable"
        );
        if let Some(existing) = state.entries.get(&auth.credential_id) {
            ensure!(
                existing.matches(auth)
                    && existing.endpoint_id == endpoint
                    && existing.command_id == command
                    && !existing.committed,
                "relay enrollment unavailable"
            );
            if let Some(transfer) = &existing.transfer {
                return Ok(transfer.clone());
            }
        } else {
            ensure!(
                state.entries.len() < MAX_ENROLLMENTS,
                "relay enrollment capacity reached"
            );
            let mut next = state.clone();
            next.entries.insert(
                auth.credential_id.clone(),
                Enrollment {
                    credential_id: auth.credential_id.clone(),
                    endpoint_id: endpoint.to_owned(),
                    auth_epoch: auth.auth_epoch,
                    runtime_ids: auth.selected_runtime_ids.clone(),
                    scopes: auth.granted_scopes.clone(),
                    command_id: command.to_owned(),
                    provisioning_key: RelaySecret(format!("txn_{}", random_hex())),
                    installation_id: None,
                    write_capability: None,
                    transfer: None,
                    committed: false,
                    latest_cursor: 0,
                    published_barrier: None,
                    pending: None,
                },
            );
            self.persist(&next)?;
            *state = next;
        }
        let pending = state
            .entries
            .get(&auth.credential_id)
            .expect("staged enrollment");
        let token = read_private(&self.config.bootstrap_token_file, 4096)?;
        let token = std::str::from_utf8(&token)?.trim();
        ensure!(valid_capability(token), "invalid relay bootstrap token");
        let request =
            serde_json::json!({"schema_version": 1, "idempotency_key": pending.provisioning_key.0});
        let issued: IssuedInstallation = self
            .json_request(
                self.client
                    .post(self.url("v1/installations")?)
                    .bearer_auth(token)
                    .json(&request),
            )
            .await?;
        ensure!(
            issued.schema_version == 1
                && valid_relay_id(&issued.installation_id)
                && valid_capability(&issued.write_capability.0)
                && valid_capability(&issued.read_capability.0)
                && valid_capability(&issued.manage_capability.0),
            "invalid relay provisioning response"
        );
        ensure!(
            issued.write_capability.0 != issued.read_capability.0
                && issued.write_capability.0 != issued.manage_capability.0
                && issued.read_capability.0 != issued.manage_capability.0,
            "relay capabilities must be distinct"
        );
        let transfer = RelayEnrollment {
            relay_origin: self.origin.as_str().trim_end_matches('/').to_owned(),
            installation_id: issued.installation_id.clone(),
            command_id: command.to_owned(),
            read_capability: issued.read_capability,
            manage_capability: issued.manage_capability,
        };
        let mut next = state.clone();
        let entry = next
            .entries
            .get_mut(&auth.credential_id)
            .expect("staged enrollment");
        entry.installation_id = Some(issued.installation_id);
        entry.write_capability = Some(issued.write_capability);
        entry.transfer = Some(transfer.clone());
        self.persist(&next)?;
        *state = next;
        Ok(transfer)
    }

    pub async fn commit(
        &self,
        auth: &AuthorizationContextV2,
        installation: &str,
        command: &str,
    ) -> anyhow::Result<RelayCommit> {
        let mut state = self.journal.lock().await;
        ensure!(
            !self.unavailable.load(Ordering::Acquire),
            "relay custody unavailable"
        );
        let entry = state
            .entries
            .get(&auth.credential_id)
            .ok_or_else(|| anyhow!("relay enrollment unavailable"))?;
        ensure!(
            entry.matches(auth)
                && entry.installation_id.as_deref() == Some(installation)
                && entry.command_id == command,
            "relay enrollment unavailable"
        );
        if !entry.committed {
            let mut next = state.clone();
            let entry = next
                .entries
                .get_mut(&auth.credential_id)
                .expect("enrollment");
            entry.transfer = None;
            entry.provisioning_key.0.zeroize();
            entry.committed = true;
            self.persist(&next)?;
            *state = next;
        }
        Ok(RelayCommit {
            installation_id: installation.to_owned(),
            command_id: command.to_owned(),
        })
    }

    pub async fn barrier(
        &self,
        auth: &AuthorizationContextV2,
        installation: &str,
        cursor: u64,
    ) -> anyhow::Result<RelayBarrier> {
        let state = self.journal.lock().await;
        ensure!(
            !self.unavailable.load(Ordering::Acquire),
            "relay custody unavailable"
        );
        let entry = state
            .entries
            .get(&auth.credential_id)
            .ok_or_else(|| anyhow!("relay enrollment unavailable"))?;
        ensure!(
            entry.matches(auth)
                && entry.installation_id.as_deref() == Some(installation)
                && entry.latest_cursor >= cursor,
            "unpublished relay cursor"
        );
        Ok(self.capture(entry, cursor))
    }

    fn capture(&self, entry: &Enrollment, through_cursor: u64) -> RelayBarrier {
        let mut runtime_ids = entry.runtime_ids.clone();
        runtime_ids.sort();
        let sessions = self.sessions.snapshot();
        let runtime_states: Vec<_> = runtime_ids
            .iter()
            .map(|runtime| {
                let state = sessions.iter().find(|session| {
                    session.node_id == entry.credential_id && session.agent == runtime
                });
                let (session_id, state_revision) = state
                    .map(|s| s.state_barrier())
                    .map(|(instance, revision)| (instance.to_string(), revision))
                    .unwrap_or(("absent".to_owned(), 0));
                RelayRuntimeState {
                    runtime_id: runtime.clone(),
                    session_id,
                    state_revision,
                }
            })
            .collect();
        let installation_id = entry.installation_id.clone().expect("issued installation");
        let mut hash = Sha256::new();
        for field in [
            &self.host_epoch,
            &installation_id,
            &entry.credential_id,
            &entry.auth_epoch.to_string(),
        ] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        for runtime in &runtime_states {
            for field in [
                &runtime.runtime_id,
                &runtime.session_id,
                &runtime.state_revision.to_string(),
            ] {
                hash.update((field.len() as u64).to_be_bytes());
                hash.update(field.as_bytes());
            }
        }
        RelayBarrier {
            installation_id,
            through_cursor,
            barrier_id: hex::encode(hash.finalize()),
            runtime_ids,
            host_epoch: self.host_epoch.clone(),
            runtime_states,
        }
    }

    pub async fn tick(&self, pairing: &PairingManager) -> anyhow::Result<()> {
        let ids: Vec<_> = self.journal.lock().await.entries.keys().cloned().collect();
        let mut failed = false;
        for id in ids {
            // Same revocation read fence as authenticated control commands. A
            // device revocation cannot race new publication after its commit.
            let (auth, endpoint) = {
                let state = self.journal.lock().await;
                let entry = &state.entries[&id];
                (entry.authorization(), entry.endpoint_id.clone())
            };
            let Ok(_permit) = pairing.prepare_connect_start(&auth, &endpoint).await else {
                continue;
            };
            failed |= self.publish_one(&auth).await.is_err();
        }
        ensure!(!failed, "relay publication pending retry");
        Ok(())
    }

    async fn publish_one(&self, auth: &AuthorizationContextV2) -> anyhow::Result<()> {
        let mut state = self.journal.lock().await;
        ensure!(
            !self.unavailable.load(Ordering::Acquire),
            "relay custody unavailable"
        );
        let Some(entry) = state.entries.get(&auth.credential_id) else {
            return Ok(());
        };
        if !entry.committed || entry.installation_id.is_none() {
            return Ok(());
        }
        let barrier = self.capture(entry, entry.latest_cursor);
        if entry.pending.is_none()
            && entry.published_barrier.as_deref() == Some(&barrier.barrier_id)
        {
            return Ok(());
        }
        if entry.pending.is_none() {
            let event_id = format!("evt_{}", random_hex());
            let mut nonce = [0u8; 12];
            rand::rngs::OsRng.fill_bytes(&mut nonce);
            let cipher = Aes256Gcm::new_from_slice(self.encryption_key.as_ref())
                .map_err(|_| anyhow!("relay cipher unavailable"))?;
            let encrypted = cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: b"remora-state-changed-v1",
                        aad: event_id.as_bytes(),
                    },
                )
                .map_err(|_| anyhow!("relay encryption failed"))?;
            let mut ciphertext = nonce.to_vec();
            ciphertext.extend(encrypted);
            let mut next = state.clone();
            next.entries
                .get_mut(&auth.credential_id)
                .expect("enrollment")
                .pending = Some(PendingEvent {
                event_id,
                event_class: "state_changed".to_owned(),
                expires_at_ms: now_ms()?
                    .checked_add(86_400_000)
                    .ok_or_else(|| anyhow!("clock overflow"))?,
                ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
                barrier: barrier.barrier_id,
            });
            self.persist(&next)?;
            *state = next;
        }
        let entry = state.entries.get(&auth.credential_id).expect("enrollment");
        let pending = entry.pending.as_ref().expect("pending event");
        // Do not send the private barrier binding as a relay-visible field.
        let body = serde_json::json!({"event_id": pending.event_id, "event_class": pending.event_class,
            "expires_at_ms": pending.expires_at_ms, "ciphertext": pending.ciphertext});
        let path = format!(
            "v1/installations/{}/events",
            entry.installation_id.as_deref().expect("installation")
        );
        let result = self
            .json_request::<Published>(
                self.client
                    .post(self.url(&path)?)
                    .bearer_auth(&entry.write_capability.as_ref().expect("write capability").0)
                    .json(&body),
            )
            .await;
        // The service resolves exact receipt replay before expiry validation.
        // An expired unaccepted wake may be superseded only after a definite
        // rejection; an ambiguous transport failure retains the exact body.
        if result
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<HttpStatus>())
            .is_some_and(|status| status.0 == 400)
            && pending.expires_at_ms <= now_ms()?
        {
            let mut next = state.clone();
            let entry = next
                .entries
                .get_mut(&auth.credential_id)
                .expect("enrollment");
            entry.pending = None;
            entry.published_barrier = None;
            self.persist(&next)?;
            *state = next;
            return Err(anyhow!("expired unaccepted wake scheduled for replacement"));
        }
        let published = result?;
        ensure!(
            published.schema_version == 1
                && published.event_id == pending.event_id
                && published.cursor > entry.latest_cursor,
            "invalid relay publication receipt"
        );
        let _ = published.replayed;
        let mut next = state.clone();
        let entry = next
            .entries
            .get_mut(&auth.credential_id)
            .expect("enrollment");
        entry.latest_cursor = published.cursor;
        entry.published_barrier = entry.pending.take().map(|event| event.barrier);
        self.persist(&next)?;
        *state = next;
        Ok(())
    }

    fn persist(&self, journal: &Journal) -> anyhow::Result<()> {
        let bytes = Zeroizing::new(serde_json::to_vec(journal)?);
        ensure!(
            bytes.len() as u64 <= MAX_JOURNAL,
            "relay journal capacity reached"
        );
        let result = write_private(&self.path, &bytes);
        if result.is_err() {
            self.unavailable.store(true, Ordering::Release);
        }
        result
    }

    fn url(&self, path: &str) -> anyhow::Result<reqwest::Url> {
        Ok(self.origin.join(path)?)
    }

    async fn json_request<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> anyhow::Result<T> {
        let response = request
            .send()
            .await
            .map_err(|_| anyhow!("relay transport unavailable"))?;
        if !response.status().is_success() {
            return Err(HttpStatus(response.status().as_u16()).into());
        }
        ensure!(
            response
                .content_length()
                .is_none_or(|size| size <= MAX_BODY as u64),
            "relay response too large"
        );
        let mut stream = response.bytes_stream();
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| anyhow!("relay response unavailable"))?;
            ensure!(
                bytes.len() + chunk.len() <= MAX_BODY,
                "relay response too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid relay response"))
    }
}

pub(crate) fn valid_relay_id(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn valid_capability(value: &str) -> bool {
    (32..=256).contains(&value.len()) && !value.chars().any(char::is_whitespace)
}
fn random_hex() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn now_ms() -> anyhow::Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

fn validate_origin(value: &str, allow_loopback_http: bool) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback());
    ensure!(
        url.scheme() == "https" || (allow_loopback_http && loopback && url.scheme() == "http"),
        "relay requires HTTPS or explicit literal loopback HTTP"
    );
    ensure!(
        url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "relay must be an origin"
    );
    Ok(url)
}

#[cfg(not(windows))]
fn read_private(path: &Path, limit: u64) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "invalid relay custody file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.mode() & 0o077 == 0 && metadata.uid() == unsafe { libc::geteuid() },
            "insecure relay custody permissions"
        );
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "relay custody file too large");
    Ok(bytes)
}

#[cfg(not(windows))]
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("invalid relay custody path"))?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let pending = parent.join(format!(".relay-{}.pending", random_hex()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| -> anyhow::Result<()> {
        let mut file = options.open(&pending)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&pending, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if pending.exists() {
        let _ = std::fs::remove_file(&pending);
    }
    result
}

#[cfg(test)]
#[path = "background_relay_tests.rs"]
mod tests;
