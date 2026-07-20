use crate::types::{Character, Monster, Resource};

/// Shared level-gap → XP-multiplier tiering used by every XP formula in this crate (combat,
/// gathering, crafting): same level or lower than the target → full XP, 5-9 levels above → 70%,
/// 10+ levels above → none. ASSUMPTION for the crafting formula specifically: the game's own spec
/// only confirms the two endpoints (≤4 diff → 1.0, ≥10 diff → 0.0); the 0.7 middle tier is carried
/// over from this codebase's existing convention (already used identically for combat/gathering)
/// rather than independently verified for crafting.
pub(crate) fn level_penalty_tier(level_diff: i32) -> f64 {
    if level_diff >= 10 { 0.0 } else if level_diff >= 5 { 0.7 } else { 1.0 }
}

pub fn calculate_cooldown(turns: i32, haste: i32) -> f64 {
    let base = turns as f64 * 2.0;
    base - (haste as f64 * 0.01) * base
}

pub fn calculate_xp(character: &Character, monster: &Monster) -> f64 {
    let level_penalty = level_penalty_tier(character.level - monster.level);

    let monster_multiplier = match monster.monster_type.as_str() {
        "elite" => 1.4,
        "boss"  => 2.0,
        _       => 1.0,
    };

    let wisdom_bonus = 1.0 + character.wisdom as f64 * 0.001;

    let base = (monster.level as f64 / character.level as f64) * 20.0
        + monster.hp as f64 * 0.04;
    (base * level_penalty * monster_multiplier * wisdom_bonus).round()
}

pub fn calculate_gathering_cooldown(resource_level: i32) -> f64 {
    30.0 + resource_level as f64 / 2.0
}

pub fn calculate_gathering_xp(character_skill_level: i32, resource: &Resource) -> f64 {
    let level_penalty = level_penalty_tier(character_skill_level - resource.level);
    let base = resource.level as f64 * 10.0;
    (base * level_penalty).round()
}
