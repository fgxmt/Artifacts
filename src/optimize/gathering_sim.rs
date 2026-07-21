use crate::api::GameData;
use crate::formulas::{calculate_crafting_xp, calculate_gathering_cooldown, calculate_gathering_xp};
use crate::types::{Item, MonsterDrop, Resource};

use super::skills::refining_skill_for;

/// Flat crafting/refining cooldown, in seconds, per unit crafted — confirmed exact game formula,
/// independent of item level, skill level, haste, or any other factor.
const CRAFT_COOLDOWN_PER_UNIT_SECS: f64 = 5.0;

/// Expected quantity of `drop` obtained per single gather action. Real game formula: drop
/// probability = (1/rate) * (1 + prospecting/1000), and each successful drop yields between
/// `min_quantity` and `max_quantity` (averaged here).
pub(crate) fn average_drop_yield(drop: &MonsterDrop, prospecting: i32) -> f64 {
    let probability  = (1.0 / drop.rate.max(1) as f64) * (1.0 + prospecting as f64 / 1000.0);
    let avg_quantity = (drop.min_quantity + drop.max_quantity) as f64 / 2.0;
    probability * avg_quantity
}

/// Highest-level `refine_skill` recipe craftable at `refining_skill_level` that consumes at least
/// one of `resource`'s own drop codes — mirrors `plan_refining_batch`'s highest-level-first recipe
/// selection (loops.rs), narrowed to recipes actually sourced from this specific resource.
fn best_refining_recipe_for<'a>(
    data: &'a GameData,
    refine_skill: &str,
    refining_skill_level: i32,
    resource: &Resource,
) -> Option<&'a Item> {
    data.items.iter()
        .filter(|i| i.craft.as_ref().is_some_and(|c| c.skill.as_deref() == Some(refine_skill)))
        .filter(|i| i.craft.as_ref().unwrap().level.is_none_or(|lvl| refining_skill_level >= lvl))
        .filter(|i| i.craft.as_ref().unwrap().items.iter()
            .any(|ing| resource.drops.iter().any(|d| d.code == ing.code)))
        .max_by_key(|i| i.level)
}

/// XP/hour for gathering `resource`, blended across a full gather-then-refine cycle: gathering a
/// skill's raw material also earns XP by refining it into its processed form (a craft action under
/// the hood — see `calculate_crafting_xp`'s doc comment), so the "true" skill XP/hour includes
/// both legs, not just the raw gather action. Falls back to the raw-gather-only figure if there's
/// no refining outlet for this resource/skill (e.g. fishing has none for its raw catches beyond
/// cooking, alchemy resources aren't refined at all).
#[allow(clippy::too_many_arguments)]
pub(crate) fn gathering_xp_per_hour(
    char_skill_level: i32,
    resource: &Resource,
    wisdom: i32,
    cooldown_pct: i32,
    data: &GameData,
    gather_skill: &str,
    refining_skill_level: i32,
    prospecting: i32,
) -> f64 {
    let gather_xp      = calculate_gathering_xp(char_skill_level, resource);
    let wisdom_bonus   = 1.0 + wisdom as f64 * 0.001;
    let base_cooldown  = calculate_gathering_cooldown(resource.level);
    let gather_cooldown = (base_cooldown * (1.0 + cooldown_pct as f64 / 100.0)).max(0.0);
    if gather_cooldown <= 0.0 { return 0.0; }

    let raw_only = (gather_xp * wisdom_bonus) / (gather_cooldown / 3600.0);

    let refine_skill = refining_skill_for(gather_skill);
    if refine_skill.is_empty() { return raw_only; }
    let Some(recipe) = best_refining_recipe_for(data, refine_skill, refining_skill_level, resource) else {
        return raw_only;
    };
    let craft = recipe.craft.as_ref().unwrap();

    let raw_needed_per_refine: i32 = craft.items.iter()
        .filter(|ing| resource.drops.iter().any(|d| d.code == ing.code))
        .map(|ing| ing.quantity)
        .sum();
    let yield_per_gather: f64 = resource.drops.iter()
        .filter(|d| craft.items.iter().any(|ing| ing.code == d.code))
        .map(|d| average_drop_yield(d, prospecting))
        .sum();
    if raw_needed_per_refine <= 0 || yield_per_gather <= 0.0 { return raw_only; }

    let gathers_per_refine = (raw_needed_per_refine as f64 / yield_per_gather).ceil().max(1.0);
    // calculate_crafting_xp already bakes in its own wisdom bonus — not reapplied here.
    let refine_xp       = calculate_crafting_xp(refining_skill_level, recipe, refine_skill, wisdom);
    let refine_cooldown = CRAFT_COOLDOWN_PER_UNIT_SECS;

    let cycle_seconds = gathers_per_refine * gather_cooldown + refine_cooldown;
    let cycle_xp      = gathers_per_refine * gather_xp * wisdom_bonus + refine_xp;
    if cycle_seconds <= 0.0 { return 0.0; }
    cycle_xp / (cycle_seconds / 3600.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real drop-rate formula: probability = (1/rate) * (1 + prospecting/1000), each hit
    /// yielding between min_quantity and max_quantity. Matches the game's own King Slime example:
    /// king_slimeball drops at rate 12, so at 100 prospecting the chance per action is
    /// (1/12) * 1.1.
    #[test]
    fn average_drop_yield_matches_king_slime_example() {
        let drop = MonsterDrop { code: "king_slimeball".into(), rate: 12, min_quantity: 1, max_quantity: 2 };
        let expected_probability = (1.0 / 12.0) * 1.1;
        let expected_yield = expected_probability * 1.5; // avg of min/max quantity
        assert!((average_drop_yield(&drop, 100) - expected_yield).abs() < 1e-9);

        // 0 prospecting: probability collapses to plain 1/rate.
        let expected_yield_bare = (1.0 / 12.0) * 1.5;
        assert!((average_drop_yield(&drop, 0) - expected_yield_bare).abs() < 1e-9);
    }

    /// Sanity check against the real game data (bundled resources.json/items.json, no network
    /// calls) rather than synthetic fixtures. Doesn't assert an exact XP/hour figure — only that
    /// the blended figure is a real, bounded improvement over gathering alone whenever a resource
    /// has a genuine refining outlet, matching hand-computed expected values for copper_rocks/
    /// copper_bar at level 1, 0 prospecting: raw-gather-only ~1180 XP/hr, blended ~1253 XP/hr.
    #[test]
    fn gathering_xp_per_hour_blends_in_refining_xp_for_real_data() {
        let items_json = std::fs::read_to_string("items.json").expect("items.json should be present at repo root");
        let resources_json = std::fs::read_to_string("resources.json").expect("resources.json should be present at repo root");
        let items: Vec<Item> = serde_json::from_str(&items_json).unwrap();
        let resources: Vec<Resource> = serde_json::from_str(&resources_json).unwrap();
        let data = GameData { monsters: vec![], items, resources, maps: vec![], craftable_equip: vec![] };

        let copper_rocks = data.resources.iter().find(|r| r.code == "copper_rocks").expect("copper_rocks should exist in resources.json");

        let blended  = gathering_xp_per_hour(1, copper_rocks, 0, 0, &data, "mining", 1, 0);
        let raw_only = calculate_gathering_xp(1, copper_rocks) / (calculate_gathering_cooldown(copper_rocks.level) / 3600.0);

        assert!(blended > raw_only, "blended ({blended}) should exceed raw-gather-only ({raw_only}) when a refining outlet exists");
        // Loose bounds around the hand-computed ~1253/~1180 (real recipe data could shift
        // slightly if the bundled JSON is regenerated, so this isn't pinned to an exact value).
        assert!((1000.0..1500.0).contains(&blended), "blended XP/hr out of expected range: {blended}");
        assert!((1000.0..1400.0).contains(&raw_only), "raw-only XP/hr out of expected range: {raw_only}");
    }
}
