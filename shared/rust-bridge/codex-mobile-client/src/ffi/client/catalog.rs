use crate::types;
use codex_app_server_protocol as upstream;
use std::collections::HashSet;

const AMP_VISIBLE_MODES: [&str; 3] = ["smart", "rush", "deep"];

fn normalize_amp_mode_name(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("amp/")
        .trim_start_matches("amp:")
        .to_ascii_lowercase()
}

fn amp_mode_description(mode: &str) -> &'static str {
    match mode {
        "smart" => "Balanced Amp mode for everyday coding tasks.",
        "rush" => "Faster Amp mode for quick edits and short answers.",
        "deep" => "Deeper Amp mode for complex implementation and debugging.",
        _ => "Amp agent mode.",
    }
}

fn amp_mode_reasoning_efforts(
    mode: &str,
) -> (Vec<types::ReasoningEffortOption>, types::ReasoningEffort) {
    let efforts = match mode {
        "smart" => vec![
            types::ReasoningEffort::High,
            types::ReasoningEffort::XHigh,
            types::ReasoningEffort::Max,
        ],
        "deep" => vec![
            types::ReasoningEffort::Low,
            types::ReasoningEffort::Medium,
            types::ReasoningEffort::XHigh,
        ],
        _ => Vec::new(),
    };
    let default = match mode {
        "smart" => types::ReasoningEffort::High,
        "deep" => types::ReasoningEffort::Medium,
        _ => types::ReasoningEffort::None,
    };
    (
        efforts
            .into_iter()
            .map(|reasoning_effort| types::ReasoningEffortOption {
                reasoning_effort,
                description: String::new(),
            })
            .collect(),
        default,
    )
}

fn amp_mode_models() -> Vec<types::ModelInfo> {
    AMP_VISIBLE_MODES
        .into_iter()
        .map(|mode| {
            let (supported_reasoning_efforts, default_reasoning_effort) =
                amp_mode_reasoning_efforts(mode);
            types::ModelInfo {
                id: mode.to_string(),
                model: mode.to_string(),
                upgrade: None,
                upgrade_model: None,
                upgrade_copy: None,
                model_link: None,
                migration_markdown: None,
                availability_nux_message: None,
                display_name: mode.to_string(),
                description: amp_mode_description(mode).to_string(),
                hidden: false,
                supported_reasoning_efforts,
                default_reasoning_effort,
                input_modalities: vec![types::InputModality::Text],
                supports_personality: false,
                is_default: mode == "smart",
                agent_runtime_kind: "amp".to_string(),
            }
        })
        .collect()
}

pub(super) fn append_missing_amp_mode_models(models: &mut Vec<types::ModelInfo>) {
    for mode in amp_mode_models() {
        let mode_name = mode.id.clone();
        let prefixed_mode = format!("amp/{mode_name}");
        let exists = models.iter().any(|existing| {
            if existing.agent_runtime_kind != "amp".to_string() {
                return false;
            }
            let id = existing.id.trim().to_ascii_lowercase();
            let model = existing.model.trim().to_ascii_lowercase();
            id == mode_name || id == prefixed_mode || model == mode_name || model == prefixed_mode
        });
        if !exists {
            models.push(mode);
        }
    }
}

pub(super) fn normalize_model_info_for_runtime(
    model_info: &mut types::ModelInfo,
    runtime_kind: types::AgentRuntimeKind,
) -> bool {
    let is_amp = runtime_kind == "amp";
    model_info.agent_runtime_kind = runtime_kind;
    if is_amp {
        let id_mode = normalize_amp_mode_name(&model_info.id);
        let mode = if id_mode.is_empty() {
            normalize_amp_mode_name(&model_info.model)
        } else {
            id_mode
        };
        if !AMP_VISIBLE_MODES.contains(&mode.as_str()) {
            return false;
        }
        let (supported_reasoning_efforts, default_reasoning_effort) =
            amp_mode_reasoning_efforts(&mode);
        model_info.id = mode.clone();
        model_info.model = mode.clone();
        model_info.display_name = mode.clone();
        model_info.description = amp_mode_description(&mode).to_string();
        model_info.hidden = false;
        model_info.supported_reasoning_efforts = supported_reasoning_efforts;
        model_info.default_reasoning_effort = default_reasoning_effort;
        model_info.is_default = mode == "smart";
    }
    true
}

pub(super) fn runtime_exposes_model_choices(runtime_kind: &str) -> bool {
    !matches!(runtime_kind, "shell")
}

pub(super) fn append_cached_models_for_failed_runtimes(
    models: &mut Vec<types::ModelInfo>,
    seen_model_ids: &mut HashSet<(types::AgentRuntimeKind, String)>,
    cached_models: &[types::ModelInfo],
    failed_runtime_kinds: &HashSet<types::AgentRuntimeKind>,
) {
    for model in cached_models {
        if !failed_runtime_kinds.contains(&model.agent_runtime_kind) {
            continue;
        }
        let dedupe_key = (model.agent_runtime_kind.clone(), model.id.clone());
        if seen_model_ids.insert(dedupe_key) {
            models.push(model.clone());
        }
    }
}

/// Flatten upstream `plugin/list` marketplaces into a deduped, sorted list of
/// `PluginSummary` rows suitable for `@`-autocomplete. Pure so it can be unit-
/// tested without running an RPC client.
pub(super) fn shape_plugin_list(
    response: upstream::PluginListResponse,
) -> Vec<types::PluginSummary> {
    let mut summaries: Vec<types::PluginSummary> = Vec::new();
    for marketplace in response.marketplaces {
        let marketplace_name = marketplace.name.trim().to_owned();
        if marketplace_name.is_empty() {
            continue;
        }
        let marketplace_path = marketplace.path.map(types::AbsolutePath::from);
        for plugin in marketplace.plugins {
            if plugin.name.trim().is_empty() {
                continue;
            }
            let summary = types::PluginSummary::from_upstream(
                marketplace_name.clone(),
                marketplace_path.clone(),
                plugin,
            );
            if summary.is_available_for_mention() {
                summaries.push(summary);
            }
        }
    }

    // Dedupe by mention_path, keeping the first occurrence.
    let mut seen: HashSet<String> = HashSet::new();
    summaries.retain(|s| seen.insert(s.mention_path.clone()));

    summaries.sort_by(|a, b| {
        a.display_title
            .to_lowercase()
            .cmp(&b.display_title.to_lowercase())
    });

    summaries
}

pub(super) fn is_mobile_hidden_skill(skill: &types::SkillMetadata) -> bool {
    if skill.name.trim().eq_ignore_ascii_case("imagegen") {
        return true;
    }

    // Temporary mobile filter: avoid steering image requests into the system
    // imagegen skill while native upstream image_generation is available.
    let mut previous = None;
    for component in skill.path.value.trim_end_matches('/').split('/') {
        if previous == Some(".system") && component.eq_ignore_ascii_case("imagegen") {
            return true;
        }
        previous = Some(component);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::models::{AbsolutePath, SkillMetadata, SkillScope};
    use crate::types::{AgentRuntimeKind, ModelInfo, ReasoningEffort, ReasoningEffortOption};

    fn skill_metadata(name: &str, path: &str) -> SkillMetadata {
        SkillMetadata {
            name: name.to_string(),
            description: String::new(),
            short_description: None,
            interface: None,
            dependencies: None,
            path: AbsolutePath {
                value: path.to_string(),
            },
            scope: SkillScope::System,
            enabled: true,
        }
    }

    fn test_model(id: &str, runtime_kind: AgentRuntimeKind) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            model: id.to_string(),
            upgrade: None,
            upgrade_model: None,
            upgrade_copy: None,
            model_link: None,
            migration_markdown: None,
            availability_nux_message: None,
            display_name: id.to_string(),
            description: String::new(),
            hidden: false,
            supported_reasoning_efforts: Vec::new(),
            default_reasoning_effort: ReasoningEffort::Medium,
            input_modalities: Vec::new(),
            supports_personality: false,
            is_default: false,
            agent_runtime_kind: runtime_kind,
        }
    }

    #[test]
    fn amp_mode_fallback_adds_builtin_modes() {
        let mut models = vec![test_model("gpt-5.2", "codex".to_string())];

        append_missing_amp_mode_models(&mut models);

        let amp_ids = models
            .iter()
            .filter(|model| model.agent_runtime_kind == "amp".to_string())
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(amp_ids, vec!["smart", "rush", "deep"]);
        assert_eq!(
            models
                .iter()
                .find(|model| model.id == "smart")
                .map(|model| model.is_default),
            Some(true)
        );
    }

    #[test]
    fn amp_mode_fallback_preserves_advertised_modes() {
        let mut models = vec![test_model("smart", "amp".to_string())];

        append_missing_amp_mode_models(&mut models);
        append_missing_amp_mode_models(&mut models);

        let smart_count = models
            .iter()
            .filter(|model| {
                model.agent_runtime_kind == "amp".to_string()
                    && (model.id == "smart" || model.id == "amp/smart")
            })
            .count();
        assert_eq!(smart_count, 1);
        assert!(models.iter().any(|model| model.id == "rush"));
        assert!(models.iter().any(|model| model.id == "deep"));
        assert!(!models.iter().any(|model| model.id == "large"));
    }

    #[test]
    fn amp_model_normalization_uses_amp_mode_efforts() {
        let mut model = test_model("amp/smart", "codex".to_string());
        model.supported_reasoning_efforts = vec![ReasoningEffortOption {
            reasoning_effort: ReasoningEffort::Low,
            description: "High".to_string(),
        }];
        model.default_reasoning_effort = ReasoningEffort::Low;

        assert!(normalize_model_info_for_runtime(
            &mut model,
            "amp".to_string()
        ));

        assert_eq!(model.agent_runtime_kind, "amp".to_string());
        assert_eq!(model.id, "smart");
        assert_eq!(model.display_name, "smart");
        assert_eq!(
            model
                .supported_reasoning_efforts
                .iter()
                .map(|option| option.reasoning_effort.clone())
                .collect::<Vec<_>>(),
            vec![
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
                ReasoningEffort::Max
            ]
        );
        assert_eq!(model.default_reasoning_effort, ReasoningEffort::High);
    }

    #[test]
    fn amp_model_normalization_filters_hidden_large_mode() {
        let mut model = test_model("large", "codex".to_string());

        assert!(!normalize_model_info_for_runtime(
            &mut model,
            "amp".to_string()
        ));
    }

    #[test]
    fn shell_runtime_does_not_expose_model_choices() {
        assert!(!runtime_exposes_model_choices("shell"));
        assert!(runtime_exposes_model_choices("amp"));
        assert!(runtime_exposes_model_choices("codex"));
    }

    #[test]
    fn failed_runtime_cache_preserves_only_failed_runtime_models() {
        let mut models = vec![test_model("smart", "amp".to_string())];
        let mut seen_model_ids = models
            .iter()
            .map(|model| (model.agent_runtime_kind.clone(), model.id.clone()))
            .collect::<HashSet<_>>();
        let cached_models = vec![
            test_model("opus", "claude".to_string()),
            test_model("gpt-5.5", "codex".to_string()),
            test_model("smart", "amp".to_string()),
        ];
        let failed_runtime_kinds = HashSet::from(["claude".to_string()]);

        append_cached_models_for_failed_runtimes(
            &mut models,
            &mut seen_model_ids,
            &cached_models,
            &failed_runtime_kinds,
        );

        assert!(models.iter().any(|model| {
            model.agent_runtime_kind == "claude".to_string() && model.id == "opus"
        }));
        assert!(!models.iter().any(|model| {
            model.agent_runtime_kind == "codex".to_string() && model.id == "gpt-5.5"
        }));
        assert_eq!(
            models
                .iter()
                .filter(|model| model.agent_runtime_kind == "amp".to_string() && model.id == "smart")
                .count(),
            1
        );
    }

    #[test]
    fn mobile_hides_imagegen_skill_by_name() {
        assert!(is_mobile_hidden_skill(&skill_metadata(
            "imagegen",
            "/Users/me/.codex/skills/.system/imagegen"
        )));
        assert!(is_mobile_hidden_skill(&skill_metadata(
            "ImageGen",
            "/Users/me/.codex/skills/user/imagegen"
        )));
    }

    #[test]
    fn mobile_hides_system_imagegen_skill_by_path() {
        assert!(is_mobile_hidden_skill(&skill_metadata(
            "AI Images",
            "/Users/me/.codex/skills/.system/imagegen/"
        )));
    }

    #[test]
    fn mobile_keeps_other_skills() {
        assert!(!is_mobile_hidden_skill(&skill_metadata(
            "browser",
            "/Users/me/.codex/skills/.system/browser"
        )));
    }

    mod plugin_list {
        use super::super::shape_plugin_list;
        use codex_app_server_protocol as upstream;
        use codex_utils_absolute_path::AbsolutePathBuf;

        fn iface(display_name: &str, short_description: &str) -> upstream::PluginInterface {
            upstream::PluginInterface {
                display_name: Some(display_name.into()),
                short_description: Some(short_description.into()),
                long_description: None,
                developer_name: None,
                category: None,
                capabilities: Vec::new(),
                website_url: None,
                privacy_policy_url: None,
                terms_of_service_url: None,
                default_prompt: None,
                brand_color: None,
                composer_icon: None,
                composer_icon_url: None,
                logo: None,
                logo_url: None,
                screenshots: Vec::new(),
                screenshot_urls: Vec::new(),
            }
        }

        fn summary(
            id: &str,
            name: &str,
            installed: bool,
            enabled: bool,
            install_policy: upstream::PluginInstallPolicy,
            display: Option<&str>,
        ) -> upstream::PluginSummary {
            upstream::PluginSummary {
                id: id.into(),
                remote_plugin_id: None,
                local_version: None,
                name: name.into(),
                share_context: None,
                source: upstream::PluginSource::Remote,
                installed,
                enabled,
                install_policy,
                auth_policy: upstream::PluginAuthPolicy::OnUse,
                availability: upstream::PluginAvailability::default(),
                interface: display.map(|d| iface(d, "")),
                keywords: Vec::new(),
            }
        }

        fn marketplace(
            name: &str,
            plugins: Vec<upstream::PluginSummary>,
        ) -> upstream::PluginMarketplaceEntry {
            upstream::PluginMarketplaceEntry {
                name: name.into(),
                path: Some(AbsolutePathBuf::try_from("/tmp/marketplace.json").unwrap()),
                interface: None,
                plugins,
            }
        }

        fn response(
            marketplaces: Vec<upstream::PluginMarketplaceEntry>,
        ) -> upstream::PluginListResponse {
            upstream::PluginListResponse {
                marketplaces,
                marketplace_load_errors: Vec::new(),
                featured_plugin_ids: Vec::new(),
            }
        }

        #[test]
        fn flattens_marketplaces_and_attaches_marketplace_name() {
            let response = response(vec![
                marketplace(
                    "openai-curated",
                    vec![summary(
                        "p1",
                        "computer-use",
                        true,
                        true,
                        upstream::PluginInstallPolicy::Available,
                        Some("Computer Use"),
                    )],
                ),
                marketplace(
                    "community",
                    vec![summary(
                        "p2",
                        "linear",
                        true,
                        true,
                        upstream::PluginInstallPolicy::Available,
                        Some("Linear"),
                    )],
                ),
            ]);

            let shaped = shape_plugin_list(response);
            assert_eq!(shaped.len(), 2);
            assert_eq!(shaped[0].name, "computer-use");
            assert_eq!(shaped[0].marketplace_name, "openai-curated");
            assert_eq!(
                shaped[0].mention_path,
                "plugin://computer-use@openai-curated"
            );
            assert_eq!(shaped[1].name, "linear");
            assert_eq!(shaped[1].marketplace_name, "community");
        }

        #[test]
        fn filters_unavailable_plugins() {
            let response = response(vec![marketplace(
                "openai-curated",
                vec![
                    summary(
                        "skip",
                        "not-installed",
                        false,
                        false,
                        upstream::PluginInstallPolicy::Available,
                        None,
                    ),
                    summary(
                        "keep-installed",
                        "alpha",
                        true,
                        false,
                        upstream::PluginInstallPolicy::Available,
                        None,
                    ),
                    summary(
                        "keep-default",
                        "beta",
                        false,
                        false,
                        upstream::PluginInstallPolicy::InstalledByDefault,
                        None,
                    ),
                    summary(
                        "keep-enabled",
                        "gamma",
                        false,
                        true,
                        upstream::PluginInstallPolicy::Available,
                        None,
                    ),
                ],
            )]);

            let shaped = shape_plugin_list(response);
            let names: Vec<&str> = shaped.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, vec!["alpha", "beta", "gamma"]);
        }

        #[test]
        fn dedupes_by_mention_path() {
            let response = response(vec![
                marketplace(
                    "openai-curated",
                    vec![summary(
                        "first",
                        "computer-use",
                        true,
                        true,
                        upstream::PluginInstallPolicy::Available,
                        Some("Computer Use (first)"),
                    )],
                ),
                marketplace(
                    "openai-curated",
                    vec![summary(
                        "second",
                        "computer-use",
                        true,
                        true,
                        upstream::PluginInstallPolicy::Available,
                        Some("Computer Use (second)"),
                    )],
                ),
            ]);

            let shaped = shape_plugin_list(response);
            assert_eq!(shaped.len(), 1);
            assert_eq!(shaped[0].id, "first");
        }

        #[test]
        fn sorts_by_display_title_case_insensitive() {
            let response = response(vec![marketplace(
                "openai-curated",
                vec![
                    summary(
                        "z",
                        "zeta",
                        true,
                        true,
                        upstream::PluginInstallPolicy::Available,
                        Some("zeta"),
                    ),
                    summary(
                        "a",
                        "alpha",
                        true,
                        true,
                        upstream::PluginInstallPolicy::Available,
                        Some("Alpha"),
                    ),
                    summary(
                        "m",
                        "mike",
                        true,
                        true,
                        upstream::PluginInstallPolicy::Available,
                        Some("Mike"),
                    ),
                ],
            )]);

            let shaped = shape_plugin_list(response);
            let titles: Vec<&str> = shaped.iter().map(|s| s.display_title.as_str()).collect();
            assert_eq!(titles, vec!["Alpha", "Mike", "zeta"]);
        }

        #[test]
        fn falls_back_to_name_when_display_name_blank() {
            let response = response(vec![marketplace(
                "openai-curated",
                vec![upstream::PluginSummary {
                    interface: Some(iface("   ", "")),
                    ..summary(
                        "p",
                        "linear",
                        true,
                        true,
                        upstream::PluginInstallPolicy::Available,
                        None,
                    )
                }],
            )]);

            let shaped = shape_plugin_list(response);
            assert_eq!(shaped[0].display_title, "linear");
        }
    }
}
