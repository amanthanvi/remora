//! Global cache of agent metadata sourced from remote runtime inspection.
//! Platforms (Swift / Kotlin) read from here when they need
//! to render an agent's label, icon, sort order, BETA badge, or branch
//! on capability flags.
//!
//! The store is keyed by the lowercase agent `name` (the same string
//! a remote host advertises and uses to route connect requests). Multiple
//! servers may advertise the same agent name; the latest probe wins —
//! agents are expected to converge on identical metadata across hosts
//! built from the same runtime bridge version.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, uniffi::Record)]
pub struct AppAgentPresentation {
    pub title: Option<String>,
    pub is_beta: bool,
    pub sort_order: i32,
    pub description: Option<String>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AppAgentCapabilities {
    pub locks_reasoning_effort_after_activity: bool,
    pub visible_modes: Option<Vec<String>>,
    pub supports_ssh_bridge: bool,
    pub uses_direct_codex_port: bool,
    pub supports_thread_permission_overrides: bool,
    pub reports_effective_thread_permissions: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AppAgentMetadata {
    pub name: String,
    pub display_name: String,
    pub presentation: Option<AppAgentPresentation>,
    pub capabilities: Option<AppAgentCapabilities>,
}

#[derive(Default)]
pub struct AgentMetadataStore {
    inner: RwLock<HashMap<String, AppAgentMetadata>>,
}

impl AgentMetadataStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Replace this agent's metadata. Called whenever a probe response
    /// carries fresh data. Older hosts that omit `presentation`
    /// / `capabilities` / `icon` still overwrite the entry — clients
    /// must tolerate partial metadata.
    pub fn upsert(&self, metadata: AppAgentMetadata) {
        let key = metadata.name.to_ascii_lowercase();
        let mut guard = self.inner.write().expect("agent metadata lock");
        guard.insert(key, metadata);
    }

    pub fn upsert_all<I>(&self, entries: I)
    where
        I: IntoIterator<Item = AppAgentMetadata>,
    {
        let mut guard = self.inner.write().expect("agent metadata lock");
        for metadata in entries {
            let key = metadata.name.to_ascii_lowercase();
            guard.insert(key, metadata);
        }
    }

    pub fn get(&self, name: &str) -> Option<AppAgentMetadata> {
        let key = name.to_ascii_lowercase();
        let guard = self.inner.read().expect("agent metadata lock");
        guard.get(&key).cloned().or_else(|| {
            guard
                .values()
                .find(|metadata| {
                    canonical_agent_runtime_kind(&metadata.name, &metadata.display_name).as_deref()
                        == Some(key.as_str())
                })
                .cloned()
        })
    }

    /// All known agents in presentation-sort order. Agents without an
    /// explicit `sort_order` fall to the end, tie-broken by name.
    pub fn all_sorted(&self) -> Vec<AppAgentMetadata> {
        let guard = self.inner.read().expect("agent metadata lock");
        let mut out: Vec<AppAgentMetadata> = guard.values().cloned().collect();
        out.sort_by(|a, b| {
            let a_order = a
                .presentation
                .as_ref()
                .map(|p| p.sort_order)
                .unwrap_or(i32::MAX);
            let b_order = b
                .presentation
                .as_ref()
                .map(|p| p.sort_order)
                .unwrap_or(i32::MAX);
            a_order.cmp(&b_order).then_with(|| a.name.cmp(&b.name))
        });
        out
    }
}

fn canonical_agent_runtime_kind(name: &str, display_name: &str) -> Option<String> {
    let name = name.trim().to_ascii_lowercase();
    let display_name = display_name.trim().to_ascii_lowercase();
    let candidate = if name.is_empty() {
        display_name.as_str()
    } else {
        name.as_str()
    };
    let canonical = match candidate {
        "codex" => Some("codex"),
        "pi" | "pi.dev" | "pidev" => Some("pi"),
        "amp" | "ampcode" | "amp-code" | "amp_code" => Some("amp"),
        "opencode" | "open-code" | "open_code" => Some("opencode"),
        "claude" | "claude-code" | "claude_code" => Some("claude"),
        "droid" | "factory" | "factory-droid" | "factory_droid" => Some("droid"),
        "hermes" => Some("hermes"),
        _ if display_name == "codex" => Some("codex"),
        _ if display_name == "pi" || display_name == "pi.dev" => Some("pi"),
        _ if display_name == "amp" || display_name == "amp code" => Some("amp"),
        _ if display_name == "opencode" || display_name == "open code" => Some("opencode"),
        _ if display_name == "claude" || display_name == "claude code" => Some("claude"),
        _ if display_name == "droid"
            || display_name == "factory"
            || display_name == "factory droid" =>
        {
            Some("droid")
        }
        _ if display_name == "hermes" => Some("hermes"),
        _ => None,
    };
    canonical
        .map(ToOwned::to_owned)
        .or_else(|| (!candidate.is_empty()).then(|| candidate.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(name: &str, sort_order: i32) -> AppAgentMetadata {
        AppAgentMetadata {
            name: name.to_owned(),
            display_name: name.to_owned(),
            presentation: Some(AppAgentPresentation {
                title: None,
                is_beta: false,
                sort_order,
                description: None,
                aliases: Vec::new(),
            }),
            capabilities: None,
        }
    }

    #[test]
    fn upsert_replaces_by_lowercased_name() {
        let store = AgentMetadataStore::new();
        store.upsert(metadata("Codex", 0));
        store.upsert(metadata("codex", 5));
        let fetched = store.get("CODEX").expect("present");
        assert_eq!(fetched.presentation.unwrap().sort_order, 5);
    }

    #[test]
    fn all_sorted_orders_by_sort_order_then_name() {
        let store = AgentMetadataStore::new();
        store.upsert(metadata("zeta", 1));
        store.upsert(metadata("alpha", 1));
        store.upsert(metadata("middle", 0));
        let sorted: Vec<String> = store.all_sorted().into_iter().map(|m| m.name).collect();
        assert_eq!(sorted, vec!["middle", "alpha", "zeta"]);
    }

    #[test]
    fn get_resolves_runtime_kind_aliases() {
        let store = AgentMetadataStore::new();
        store.upsert(metadata("pi.dev", 0));
        let fetched = store.get("pi").expect("canonical alias should resolve");
        assert_eq!(fetched.name, "pi.dev");
    }
}
