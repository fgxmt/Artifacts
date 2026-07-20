use crate::types::{CraftInfo, InventorySlot, Item};

use super::xp::level_penalty_tier;

/// (xp_base, coefficient) bracket for the crafting XP formula, keyed by the crafted item's level.
fn craft_xp_bucket(item_level: i32) -> (f64, f64) {
    match item_level {
        i if i < 5  => (50.0, 25.0),
        5..=9       => (100.0, 30.0),
        10..=14     => (200.0, 35.0),
        15..=19     => (325.0, 40.0),
        20..=24     => (450.0, 45.0),
        25..=29     => (550.0, 50.0),
        30..=34     => (650.0, 55.0),
        35..=39     => (750.0, 60.0),
        40..=44     => (850.0, 65.0),
        _           => (1000.0, 70.0),
    }
}

/// Skill multiplier for the crafting XP formula. Also covers mining/woodcutting/fishing: refining
/// a raw material into its processed form (e.g. copper_ore -> copper_bar) is itself a craft action
/// in this game, with `craft.skill` set to the gathering skill's own name — this formula applies
/// there too, distinct from the raw gather action's own XP (`calculate_gathering_xp`, unchanged).
fn craft_skill_multiplier(skill: &str) -> f64 {
    match skill {
        "mining" | "woodcutting" | "fishing" => 0.1,
        "cooking" => 0.5,
        "weaponcrafting" | "gearcrafting" | "jewelrycrafting" | "alchemy" => 1.0,
        _ => 0.0,
    }
}

/// XP granted by ONE craft action (i.e. one `craft_item(..., quantity=1)` call) for `item` under
/// `skill`, at `character_skill_level` with `wisdom` — the real game formula, not an approximation.
/// Deliberately does NOT multiply by `craft.quantity` (the recipe's own output-batch size) or
/// divide by ingredient count: those are derived concerns for callers that need a per-ingredient
/// or per-batch metric (see `craft_xp_per_ingredient`), not part of the XP award itself.
pub fn calculate_crafting_xp(character_skill_level: i32, item: &Item, skill: &str, wisdom: i32) -> f64 {
    let craft = match &item.craft {
        Some(c) => c,
        None => return 0.0,
    };
    if craft.skill.as_deref() != Some(skill) {
        return 0.0;
    }

    let (xp_base, coefficient) = craft_xp_bucket(item.level);
    let level_penalty   = level_penalty_tier(character_skill_level - item.level);
    let skill_multiplier = craft_skill_multiplier(skill);
    let wisdom_bonus    = 1.0 + wisdom as f64 * 0.001;
    let skill_ratio     = item.level as f64 / character_skill_level.max(1) as f64;

    ((xp_base + skill_ratio * coefficient) * skill_multiplier * level_penalty * wisdom_bonus).round()
}

/// XP per ingredient consumed for one craft of `item` — the material-efficiency metric used to
/// rank recipes against each other (e.g. `filler_candidates`), as distinct from
/// `calculate_crafting_xp`'s raw per-craft-action XP.
pub fn craft_xp_per_ingredient(character_skill_level: i32, item: &Item, skill: &str, wisdom: i32) -> f64 {
    let xp = calculate_crafting_xp(character_skill_level, item, skill, wisdom);
    let total_ingredients: i32 = item.craft.as_ref().map(|c| c.items.iter().map(|i| i.quantity).sum()).unwrap_or(0);
    if total_ingredients <= 0 { 0.0 } else { xp / total_ingredients as f64 }
}

/// XP required to go from `level` to `level + 1` — matches `{skill}_max_xp` when the character is
/// currently at `level`. 0 past level 49 (the table has no entry for reaching level 50, the max).
pub const LEVEL_UP_XP_TABLE: [i32; 49] = [
    150, 250, 350, 450, 700, 950, 1200, 1450, 1700, 2100,
    2500, 2900, 3300, 3700, 4400, 5100, 5800, 6500, 7200, 8200,
    9200, 10200, 11200, 12200, 13400, 14600, 15800, 17000, 18200, 19700,
    21200, 22700, 24200, 25700, 27500, 29300, 31100, 32900, 34700, 36500,
    38600, 40700, 42800, 44900, 47000, 48800, 50600, 52400, 54200,
];

pub fn xp_to_next_level(level: i32) -> i32 {
    LEVEL_UP_XP_TABLE.get((level - 1) as usize).copied().unwrap_or(0)
}

pub fn craftable_quantity(inventory: &[InventorySlot], craft: &CraftInfo) -> i32 {
    craft.items.iter().map(|ing| {
        let have: i32 = inventory.iter()
            .filter(|s| s.code == ing.code)
            .map(|s| s.quantity)
            .sum();
        have / ing.quantity.max(1)
    }).min().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CraftIngredient;

    fn make_craft_item(code: &str, level: i32, skill: &str, ingredients: Vec<(&str, i32)>, recyclable: bool) -> Item {
        Item {
            name: code.into(), code: code.into(), level, item_type: "weapon".into(),
            subtype: "".into(), description: "".into(), conditions: vec![],
            effects: vec![],
            craft: Some(CraftInfo {
                skill: Some(skill.into()),
                level: None,
                items: ingredients.into_iter().map(|(c, q)| CraftIngredient { code: c.into(), quantity: q }).collect(),
                quantity: 1,
            }),
            tradeable: true,
            recyclable,
        }
    }

    #[test]
    fn xp_to_next_level_matches_table_boundaries() {
        assert_eq!(xp_to_next_level(1), 150);
        assert_eq!(xp_to_next_level(49), 54200);
        assert_eq!(xp_to_next_level(50), 0); // past the table -- max level, nothing further needed
        assert_eq!(xp_to_next_level(60), 0);
    }

    #[test]
    fn calculate_crafting_xp_matches_hand_computed_values() {
        // Bracket <5: xp_base=50, coefficient=25. Skill level 5, item level 3: level_diff=2 (<5
        // -> penalty 1.0), skill_ratio=3/5=0.6. xp = round((50 + 0.6*25) * 1.0 * 1.0 * 1.0) = 65.
        let item = make_craft_item("test_item", 3, "weaponcrafting", vec![("mat", 1)], false);
        assert_eq!(calculate_crafting_xp(5, &item, "weaponcrafting", 0), 65.0);

        // Bracket 10-14: xp_base=200, coefficient=35. Skill level 12, item level 12 (same level ->
        // penalty 1.0, skill_ratio=1.0). xp = round((200 + 35) * 1.0 * 1.0 * 1.0) = 235.
        let item2 = make_craft_item("test_item2", 12, "gearcrafting", vec![("mat", 1)], false);
        assert_eq!(calculate_crafting_xp(12, &item2, "gearcrafting", 0), 235.0);

        // Refining (mining) multiplier is 0.1. Bracket 20-24: xp_base=450, coefficient=45. Skill
        // level 20, item level 20 (skill_ratio=1.0). xp = round((450+45) * 0.1 * 1.0 * 1.0)
        // = round(49.5) = 50.
        let item3 = make_craft_item("copper_bar_like", 20, "mining", vec![("ore", 1)], false);
        assert_eq!(calculate_crafting_xp(20, &item3, "mining", 0), 50.0);

        // Wisdom bonus: +100 wisdom -> 1.1x on the first case (65 * 1.1 = 71.5 -> round = 72).
        assert_eq!(calculate_crafting_xp(5, &item, "weaponcrafting", 100), 72.0);

        // Level penalty: 10+ levels above the item -> zero XP regardless of everything else.
        let trivial = make_craft_item("trivial", 1, "weaponcrafting", vec![("mat", 1)], false);
        assert_eq!(calculate_crafting_xp(15, &trivial, "weaponcrafting", 0), 0.0);
    }
}
