use std::{collections::HashSet, fmt};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{MAX_RUNTIME_IDS, WireError, valid_idempotency, valid_runtime_id};
use crate::background_relay::{RelayInstallationId, ValidatedRelayOrigin};

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayEnrollmentV2 {
    pub(crate) relay_origin: String,
    pub(crate) installation_id: String,
    pub(crate) command_id: String,
    pub(crate) read_capability: Zeroizing<String>,
    pub(crate) manage_capability: Zeroizing<String>,
}

impl fmt::Debug for RelayEnrollmentV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RelayEnrollmentV2(<redacted>)")
    }
}

impl RelayEnrollmentV2 {
    pub(super) fn validate(&self) -> Result<(), WireError> {
        // Configuration applies the final loopback opt-in before custody.
        ValidatedRelayOrigin::parse(&self.relay_origin, true)
            .map_err(|_| WireError::InvalidResponse)?;
        validate_installation(&self.installation_id)?;
        valid_idempotency(&self.command_id).map_err(|_| WireError::InvalidResponse)?;
        for capability in [&self.read_capability, &self.manage_capability] {
            if !(32..=256).contains(&capability.len())
                || !capability
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(WireError::InvalidResponse);
            }
        }
        if self.read_capability == self.manage_capability {
            return Err(WireError::InvalidResponse);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayCommitV2 {
    pub(crate) installation_id: String,
    pub(crate) command_id: String,
}

impl RelayCommitV2 {
    pub(super) fn validate(&self) -> Result<(), WireError> {
        validate_installation(&self.installation_id)?;
        valid_idempotency(&self.command_id).map_err(|_| WireError::InvalidResponse)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayRuntimeStateV2 {
    pub(crate) runtime_id: String,
    pub(crate) session_id: String,
    pub(crate) state_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayBarrierV2 {
    pub(crate) installation_id: String,
    pub(crate) through_cursor: u64,
    pub(crate) barrier_id: String,
    pub(crate) runtime_ids: Vec<String>,
    pub(crate) host_epoch: String,
    pub(crate) runtime_states: Vec<RelayRuntimeStateV2>,
}

impl RelayBarrierV2 {
    pub(super) fn validate(&self) -> Result<(), WireError> {
        validate_installation(&self.installation_id)?;
        for digest in [&self.barrier_id, &self.host_epoch] {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(WireError::InvalidResponse);
            }
        }
        if self.runtime_ids.is_empty()
            || self.runtime_ids.len() > MAX_RUNTIME_IDS
            || self.runtime_states.len() != self.runtime_ids.len()
        {
            return Err(WireError::InvalidResponse);
        }
        let mut runtimes = HashSet::new();
        for runtime in &self.runtime_ids {
            valid_runtime_id(runtime).map_err(|_| WireError::InvalidResponse)?;
            if !runtimes.insert(runtime.as_str()) {
                return Err(WireError::InvalidResponse);
            }
        }
        let mut states = HashSet::new();
        for state in &self.runtime_states {
            if !runtimes.contains(state.runtime_id.as_str())
                || !states.insert(state.runtime_id.as_str())
                || state.session_id.is_empty()
                || state.session_id.len() > 128
                || !state
                    .session_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                || (state.session_id == "absent" && state.state_revision != 0)
            {
                return Err(WireError::InvalidResponse);
            }
        }
        Ok(())
    }
}

pub(super) fn validate_installation(value: &str) -> Result<(), WireError> {
    RelayInstallationId::parse(value)
        .map(|_| ())
        .map_err(|_| WireError::InvalidResponse)
}
