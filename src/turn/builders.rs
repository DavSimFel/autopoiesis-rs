use anyhow::Result;

use crate::context::{Identity, SessionManifest, SkillContext, SkillLoader, SubscriptionContext};
use crate::read_tool::ReadFile;
use crate::skills::SkillDefinition;
use crate::subscription::SubscriptionRecord;

use super::Turn;
use super::tiers::{TurnTier, resolve_tier};

fn identity_vars_for_turn(
    config: &crate::config::Config,
    tools: &[crate::llm::FunctionTool],
) -> std::collections::HashMap<String, String> {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(ToString::to_string))
        .unwrap_or_default();
    let tools_list = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>()
        .join(",");

    let mut vars = std::collections::HashMap::new();
    vars.insert("model".to_string(), config.model.clone());
    vars.insert("cwd".to_string(), cwd);
    vars.insert("tools".to_string(), tools_list);
    vars
}

fn add_budget_guard(turn: Turn, config: &crate::config::Config) -> Turn {
    if let Some(budget) = &config.budget
        && (budget.max_tokens_per_turn.is_some()
            || budget.max_tokens_per_session.is_some()
            || budget.max_tokens_per_day.is_some())
    {
        return turn.guard(crate::gate::BudgetGuard::new(budget.clone()));
    }

    turn
}

struct TurnBuildOptions<'a> {
    include_shell_guards: bool,
    include_delegation: bool,
    include_skills: bool,
    skill_loader: Option<Vec<SkillDefinition>>,
    session_manifest: Option<SessionManifest>,
    subscriptions: &'a [SubscriptionRecord],
}

fn build_turn_with_tool(
    config: &crate::config::Config,
    tool: impl crate::tool::Tool + 'static,
    options: TurnBuildOptions<'_>,
) -> Result<Turn> {
    let tool_definition = tool.definition();
    let vars = identity_vars_for_turn(config, std::slice::from_ref(&tool_definition));
    let identity_prompt =
        crate::identity::load_system_prompt_from_files(&config.identity_files, &vars)?;
    let mut turn = Turn::new()
        .context(Identity::new(config.identity_files.clone(), vars, &identity_prompt).strict());
    if options.include_skills {
        turn = turn.context(SkillContext::new(config.skills.browse()));
    }
    if let Some(skills) = options.skill_loader {
        turn = turn.context(SkillLoader::new(skills));
    }
    if let Some(session_manifest) = options.session_manifest {
        turn = turn.context(session_manifest);
    }
    turn = turn.context(SubscriptionContext::new(
        options.subscriptions.to_vec(),
        config.subscriptions.context_token_budget,
    ));
    turn = turn.tool(tool);
    turn = add_budget_guard(turn, config).guard(crate::gate::SecretRedactor::default_catalog());

    if options.include_shell_guards {
        turn = turn
            .guard(crate::gate::ShellSafety::with_policy_and_skills_dirs(
                config.shell_policy.clone(),
                vec![
                    config.skills_dir.clone(),
                    config.skills_dir_resolved.clone(),
                ],
            ))
            .guard(crate::gate::ExfilDetector::with_skills_dirs(vec![
                config.skills_dir.clone(),
                config.skills_dir_resolved.clone(),
            ]));
    }

    if options.include_delegation
        && let Some(tier) = config.active_t1_config()
        && (tier.delegation_token_threshold.is_some() || tier.delegation_tool_depth.is_some())
    {
        turn = turn.delegation(crate::delegation::DelegationConfig {
            token_threshold: tier.delegation_token_threshold,
            tool_depth_threshold: tier.delegation_tool_depth,
        });
    }

    Ok(turn)
}

/// Build the default T1 turn, kept for backward compatibility.
pub fn build_default_turn(config: &crate::config::Config) -> Result<Turn> {
    build_turn_with_tool(
        config,
        crate::tool::Shell::with_limits(
            config.shell_policy.max_output_bytes,
            config.shell_policy.max_timeout_ms,
        ),
        TurnBuildOptions {
            include_shell_guards: true,
            include_delegation: true,
            include_skills: true,
            skill_loader: None,
            session_manifest: None,
            subscriptions: &[],
        },
    )
}

/// Build the active tier turn using the config's resolved agent tier.
pub fn build_turn_for_config(config: &crate::config::Config) -> Result<Turn> {
    build_turn_for_config_with_subscriptions(config, &[])
}

/// Build the active tier turn with explicit active subscriptions.
pub fn build_turn_for_config_with_subscriptions(
    config: &crate::config::Config,
    subscriptions: &[SubscriptionRecord],
) -> Result<Turn> {
    build_turn_for_config_with_subscriptions_and_manifest(config, subscriptions, None)
}

/// Build the active tier turn with explicit active subscriptions and an optional session manifest.
pub fn build_turn_for_config_with_subscriptions_and_manifest(
    config: &crate::config::Config,
    subscriptions: &[SubscriptionRecord],
    session_manifest: Option<&SessionManifest>,
) -> Result<Turn> {
    match resolve_tier(config) {
        TurnTier::T1 => build_turn_with_tool(
            config,
            crate::tool::Shell::with_limits(
                config.shell_policy.max_output_bytes,
                config.shell_policy.max_timeout_ms,
            ),
            TurnBuildOptions {
                include_shell_guards: true,
                include_delegation: true,
                include_skills: true,
                skill_loader: None,
                session_manifest: session_manifest.cloned(),
                subscriptions,
            },
        ),
        TurnTier::T2 => build_t2_turn_with_subscriptions(config, subscriptions, session_manifest),
        TurnTier::T3 => build_t3_turn_with_subscriptions(config, subscriptions, session_manifest),
    }
}

/// Build a T2 turn with structured reads only.
pub fn build_t2_turn(config: &crate::config::Config) -> Result<Turn> {
    build_t2_turn_with_subscriptions(config, &[], None)
}

fn build_t2_turn_with_subscriptions(
    config: &crate::config::Config,
    subscriptions: &[SubscriptionRecord],
    session_manifest: Option<&SessionManifest>,
) -> Result<Turn> {
    build_turn_with_tool(
        config,
        ReadFile::from_config_with_protected_paths(
            &config.read,
            vec![
                config.skills_dir.clone(),
                config.skills_dir_resolved.clone(),
            ],
        ),
        TurnBuildOptions {
            include_shell_guards: false,
            include_delegation: false,
            include_skills: true,
            skill_loader: None,
            session_manifest: session_manifest.cloned(),
            subscriptions,
        },
    )
}

/// Build a T3 turn with shell access but no delegation hint support.
pub fn build_t3_turn(config: &crate::config::Config) -> Result<Turn> {
    build_t3_turn_with_subscriptions(config, &[], None)
}

fn build_t3_turn_with_subscriptions(
    config: &crate::config::Config,
    subscriptions: &[SubscriptionRecord],
    session_manifest: Option<&SessionManifest>,
) -> Result<Turn> {
    build_turn_with_tool(
        config,
        crate::tool::Shell::with_limits(
            config.shell_policy.max_output_bytes,
            config.shell_policy.max_timeout_ms,
        ),
        TurnBuildOptions {
            include_shell_guards: true,
            include_delegation: false,
            include_skills: false,
            skill_loader: None,
            session_manifest: session_manifest.cloned(),
            subscriptions,
        },
    )
}

pub fn build_spawned_t3_turn(
    config: &crate::config::Config,
    skills: Vec<SkillDefinition>,
) -> Result<Turn> {
    build_turn_with_tool(
        config,
        crate::tool::Shell::with_limits(
            config.shell_policy.max_output_bytes,
            config.shell_policy.max_timeout_ms,
        ),
        TurnBuildOptions {
            include_shell_guards: true,
            include_delegation: false,
            include_skills: false,
            skill_loader: Some(skills),
            session_manifest: None,
            subscriptions: &[],
        },
    )
}
