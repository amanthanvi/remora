use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::api::BridgeApiClient;
use crate::cli::SpawnMode;
use crate::config::PollLoopConfig;
use crate::error::RemoteControlError;
use crate::wire::{PermissionMode, Work, WorkData, WorkSecret};

pub trait SessionSpawner: Send + Sync {
    fn spawn_session(
        &self,
        context: SessionSpawnContext,
    ) -> BoxFuture<'_, Result<ActiveRemoteSession, RemoteControlError>>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeRuntimeConfig {
    pub environment_id: String,
    pub cwd: PathBuf,
    pub spawn_mode: SpawnMode,
    pub max_sessions: usize,
    pub create_session_in_dir: bool,
    pub sandbox: bool,
    pub permission_mode: Option<PermissionMode>,
    pub debug_file: Option<PathBuf>,
    pub poll_loop: PollLoopConfig,
}

impl BridgeRuntimeConfig {
    pub fn new(environment_id: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            environment_id: environment_id.into(),
            cwd: cwd.into(),
            spawn_mode: SpawnMode::SameDir,
            max_sessions: 1,
            create_session_in_dir: true,
            sandbox: true,
            permission_mode: None,
            debug_file: None,
            poll_loop: PollLoopConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSpawnContext {
    pub work_id: String,
    pub session_id: String,
    pub secret: WorkSecret,
    pub cwd: PathBuf,
    pub spawn_mode: SpawnMode,
    pub worktree_dir_name: Option<String>,
    pub sandbox: bool,
    pub permission_mode: Option<PermissionMode>,
    pub debug_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ActiveRemoteSession {
    pub session_id: String,
    pub work_id: String,
    pub secret: WorkSecret,
    pub cwd: PathBuf,
    pub started_at: Instant,
}

impl ActiveRemoteSession {
    pub fn from_context(context: &SessionSpawnContext) -> Self {
        Self {
            session_id: context.session_id.clone(),
            work_id: context.work_id.clone(),
            secret: context.secret.clone(),
            cwd: context.cwd.clone(),
            started_at: Instant::now(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorkLoopAction {
    NoWork,
    HealthcheckAcked { work_id: String },
    UnknownWorkAcked { work_id: String },
    SessionAlreadyRunning { work_id: String, session_id: String },
    SessionSpawned { work_id: String, session_id: String },
    AtCapacity { work_id: String, capacity: usize },
}

pub struct BridgeRuntime<S> {
    client: BridgeApiClient,
    config: BridgeRuntimeConfig,
    spawner: S,
    active_sessions: HashMap<String, ActiveRemoteSession>,
}

impl<S> BridgeRuntime<S>
where
    S: SessionSpawner,
{
    pub fn new(client: BridgeApiClient, config: BridgeRuntimeConfig, spawner: S) -> Self {
        Self {
            client,
            config,
            spawner,
            active_sessions: HashMap::new(),
        }
    }

    pub fn config(&self) -> &BridgeRuntimeConfig {
        &self.config
    }

    pub fn active_sessions(&self) -> &HashMap<String, ActiveRemoteSession> {
        &self.active_sessions
    }

    pub fn mark_session_finished(&mut self, session_id: &str) -> Option<ActiveRemoteSession> {
        self.active_sessions.remove(session_id)
    }

    pub async fn process_next_work(&mut self) -> Result<WorkLoopAction, RemoteControlError> {
        let work = self
            .client
            .poll_for_work(
                &self.config.environment_id,
                Some(self.config.poll_loop.reclaim_older_than_ms),
            )
            .await?;
        match work {
            Some(work) => self.handle_work(work).await,
            None => Ok(WorkLoopAction::NoWork),
        }
    }

    pub async fn handle_work(&mut self, work: Work) -> Result<WorkLoopAction, RemoteControlError> {
        match &work.data {
            WorkData::Healthcheck { .. } => {
                self.client
                    .acknowledge_work(&self.config.environment_id, &work.id)
                    .await?;
                Ok(WorkLoopAction::HealthcheckAcked { work_id: work.id })
            }
            WorkData::Session { .. } => self.handle_session_work(work).await,
            WorkData::Unknown => {
                self.client
                    .acknowledge_work(&self.config.environment_id, &work.id)
                    .await?;
                Ok(WorkLoopAction::UnknownWorkAcked { work_id: work.id })
            }
        }
    }

    async fn handle_session_work(
        &mut self,
        work: Work,
    ) -> Result<WorkLoopAction, RemoteControlError> {
        let session_id = session_id_from_work(&work);
        let secret = work.decode_secret()?;
        if let Some(active) = self.active_sessions.get_mut(&session_id) {
            active.secret = secret;
            self.client
                .acknowledge_work(&self.config.environment_id, &work.id)
                .await?;
            return Ok(WorkLoopAction::SessionAlreadyRunning {
                work_id: work.id,
                session_id,
            });
        }
        if self.active_sessions.len() >= self.config.max_sessions {
            return Ok(WorkLoopAction::AtCapacity {
                work_id: work.id,
                capacity: self.config.max_sessions,
            });
        }

        let cwd = match self.config.spawn_mode {
            SpawnMode::SameDir | SpawnMode::Session => self.config.cwd.clone(),
            SpawnMode::Worktree => self.config.cwd.join(worktree_name_for_session(&session_id)),
        };
        let context = SessionSpawnContext {
            work_id: work.id.clone(),
            session_id: session_id.clone(),
            secret,
            cwd,
            spawn_mode: self.config.spawn_mode,
            worktree_dir_name: (self.config.spawn_mode == SpawnMode::Worktree)
                .then(|| worktree_name_for_session(&session_id)),
            sandbox: self.config.sandbox,
            permission_mode: self.config.permission_mode.clone(),
            debug_file: self.config.debug_file.clone(),
        };
        let active = self.spawner.spawn_session(context).await?;
        self.active_sessions
            .insert(active.session_id.clone(), active);
        self.client
            .acknowledge_work(&self.config.environment_id, &work.id)
            .await?;
        Ok(WorkLoopAction::SessionSpawned {
            work_id: work.id,
            session_id,
        })
    }
}

fn session_id_from_work(work: &Work) -> String {
    match &work.data {
        WorkData::Session { session_id, id, .. } => session_id
            .as_ref()
            .or(id.as_ref())
            .cloned()
            .unwrap_or_else(|| work.id.clone()),
        WorkData::Healthcheck { .. } | WorkData::Unknown => work.id.clone(),
    }
}

pub fn worktree_name_for_session(session_id: &str) -> String {
    format!("bridge-{}", sanitize_session_id(session_id))
}

fn sanitize_session_id(session_id: &str) -> String {
    let mut out = String::with_capacity(session_id.len());
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "session".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_names_are_stable_and_shell_safe() {
        assert_eq!(
            worktree_name_for_session("sess/abc:123"),
            "bridge-sess-abc-123"
        );
        assert_eq!(worktree_name_for_session("///"), "bridge-session");
    }

    #[test]
    fn session_work_prefers_session_id_then_id_then_work_id() {
        let work = Work {
            id: "work".to_string(),
            secret: "unused".to_string(),
            data: WorkData::Session {
                id: Some("legacy".to_string()),
                session_id: Some("session".to_string()),
                session_ingress_url: None,
                extra: Default::default(),
            },
        };
        assert_eq!(session_id_from_work(&work), "session");
    }
}
