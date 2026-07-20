use crate::types::{Character, Item, ItemCondition};

/// Sum of all effect values on `item` matching `code` (an item can list the same
/// effect code more than once).
pub(crate) fn item_effect_value(item: &Item, code: &str) -> i32 {
    item.effects.iter().filter(|e| e.code == code).map(|e| e.value).sum()
}

/// Character stat backing an item condition code, if we track one.
fn character_level_stat(character: &Character, code: &str) -> Option<i32> {
    Some(match code {
        "level"                => character.level,
        "woodcutting_level"    => character.woodcutting_level,
        "mining_level"         => character.mining_level,
        "fishing_level"        => character.fishing_level,
        "alchemy_level"        => character.alchemy_level,
        "weaponcrafting_level" => character.weaponcrafting_level,
        "gearcrafting_level"   => character.gearcrafting_level,
        "jewelrycrafting_level"=> character.jewelrycrafting_level,
        "cooking_level"        => character.cooking_level,
        _ => return None,
    })
}

fn condition_met(character: &Character, cond: &ItemCondition) -> bool {
    match character_level_stat(character, &cond.code) {
        Some(actual) => match cond.operator.as_str() {
            "gt"  => actual > cond.value,
            "gte" => actual >= cond.value,
            "lt"  => actual < cond.value,
            "lte" => actual <= cond.value,
            "eq"  => actual == cond.value,
            _     => false,
        },
        // Achievement-gated (e.g. "achievement_unlocked") or otherwise untracked
        // condition — we can't verify it, so treat the item as ineligible.
        None => false,
    }
}

pub(crate) fn meets_conditions(character: &Character, item: &Item) -> bool {
    item.conditions.iter().all(|c| condition_met(character, c))
}

/// Clones `character` and applies (or, with `sign = -1`, removes) `item`'s
/// combat-relevant effects. Effect codes with no known combat stat (threat,
/// prospecting, consumable buffs, ...) are ignored.
pub(crate) fn adjust_combat_stats(character: &Character, item: &Item, sign: i32) -> Character {
    let mut c = character.clone();
    for effect in &item.effects {
        let v = effect.value * sign;
        match effect.code.as_str() {
            "hp"              => c.max_hp += v,
            "dmg"             => c.dmg += v,
            "dmg_fire"        => c.dmg_fire += v,
            "dmg_earth"       => c.dmg_earth += v,
            "dmg_water"       => c.dmg_water += v,
            "dmg_air"         => c.dmg_air += v,
            "attack_fire"     => c.attack_fire += v,
            "attack_earth"    => c.attack_earth += v,
            "attack_water"    => c.attack_water += v,
            "attack_air"      => c.attack_air += v,
            "res_fire"        => c.res_fire += v,
            "res_earth"       => c.res_earth += v,
            "res_water"       => c.res_water += v,
            "res_air"         => c.res_air += v,
            "critical_strike" => c.critical_strike += v,
            "initiative"      => c.initiative += v,
            "haste"           => c.haste += v,
            "wisdom"          => c.wisdom += v,
            _ => {}
        }
    }
    c
}
