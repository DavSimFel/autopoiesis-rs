/// Turn tier resolved from the active agent configuration.
pub enum TurnTier {
    T1,
    T2,
    T3,
}

pub fn resolve_tier(config: &crate::config::Config) -> TurnTier {
    match config
        .active_agent_definition()
        .and_then(|agent| agent.tier.as_deref())
    {
        Some("t2") => TurnTier::T2,
        Some("t3") => TurnTier::T3,
        _ => TurnTier::T1,
    }
}
