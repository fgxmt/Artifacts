use std::collections::HashSet;

use reqwest::Client;

use crate::flags::GameState;
use crate::formulas::global_subtier;
use crate::optimize::{
    brute_force_optimal_loadout, find_best_material_drop_target, find_optimal_gathering,
    find_optimal_monster, optimize_combat_loadout, optimize_items, GatherTarget, ItemRole,
    MonsterTarget,
};
use crate::types::{Character, Result};

use super::movement::move_to_nearest;

/// Whether `character` (currently fighting, whether the dedicated fighter or a promoted
/// character — see `loops::promotion`) should prioritize farming wishlist-material drops over
/// plain combat XP/hour: true once its own combat level's global subtier has moved strictly past
/// the alchemy-crafting character's lowest equipment-crafting-skill level's global subtier.
fn should_farm_wishlist_drops(character: &Character, state: &GameState) -> bool {
    global_subtier(character.level) > global_subtier(state.crafter_min_gear_level())
}

/// Every direct ingredient code of the wishlist items in `item_codes` — the "materials in the
/// wishlist" that make a monster's drop worth farming for.
fn wishlist_material_codes(state: &GameState, item_codes: &[String]) -> HashSet<String> {
    item_codes.iter()
        .filter_map(|code| state.data.items.iter().find(|i| i.code == *code))
        .filter_map(|item| item.craft.as_ref())
        .flat_map(|craft| craft.items.iter().map(|ing| ing.code.clone()))
        .collect()
}

/// Picks the fight target. If `character` should be farming wishlist-material drops (see
/// `should_farm_wishlist_drops`), tries the equipment wishlist's materials first, then the
/// alchemy wishlist's as a fallback, and only falls through to plain XP/hour-optimal targeting
/// (`find_optimal_monster`) if neither turns up a guaranteed-beatable monster — or if the
/// character isn't past the drop-farming threshold at all.
async fn find_fight_target(
    client: &Client,
    name: &str,
    character: &Character,
    state: &GameState,
) -> Result<Option<MonsterTarget>> {
    if should_farm_wishlist_drops(character, state) {
        let equipment_materials = wishlist_material_codes(state, &state.wishlist_equipment_codes());
        if let Some(target) = find_best_material_drop_target(character, &state.data, &equipment_materials) {
            println!(
                "[{}] Farming {} for wishlist material drops — {:.0} XP/hr (guaranteed win from {} HP)",
                crate::ts_char(name), target.name, target.avg_xp_per_hour, target.min_hp_threshold
            );
            return Ok(Some(target));
        }

        let alchemy_materials = wishlist_material_codes(state, &state.wishlist_alchemy_codes());
        if let Some(target) = find_best_material_drop_target(character, &state.data, &alchemy_materials) {
            println!(
                "[{}] Farming {} for wishlist material drops (alchemy fallback) — {:.0} XP/hr (guaranteed win from {} HP)",
                crate::ts_char(name), target.name, target.avg_xp_per_hour, target.min_hp_threshold
            );
            return Ok(Some(target));
        }
    }

    find_optimal_monster(client, name, &state.data).await
}

pub(crate) async fn optimize_fight(
    client: &Client,
    name: &'static str,
    character: &mut Character,
    state: &GameState,
    optimize_equipment: bool,
) -> Result<Option<i32>> {
    match find_fight_target(client, name, character, state).await {
        Ok(Some(target)) => {
            move_to_nearest(client, name, character, &target.locations, &target.name).await?;
            if optimize_equipment {
                // Full joint brute-force search, same as at program init — a combat level-up can
                // change which items meet level conditions and shift the whole best-achievable-
                // XP/hour landscape, so the old joint optimum isn't necessarily still optimal.
                // This doesn't need `target` at all: which monster ends up optimal is an output
                // of the search, not an input to it.
                let ratings = optimize_combat_loadout(name, character, &state.data);
                state.set_item_ratings(name, ratings, character);
                // Ratings just changed, so an item already sitting in the bank (unchanged since
                // the last scan) can newly qualify as an upgrade — re-check and flag it now
                // rather than waiting for the bank to happen to change again.
                state.flag_bank_upgrades_for(name).await;
            }
            Ok(Some(target.min_hp_threshold))
        }
        Ok(None) => {
            eprintln!("[{}] No guaranteed-win monster found for current stats", crate::ts_char(name));
            Ok(None)
        }
        Err(e) => {
            eprintln!("[{}] Optimization error: {}", crate::ts_char(name), e);
            Ok(None)
        }
    }
}

/// Finds the optimal gathering target(s), moves to the nearest, optionally re-runs item
/// optimization, and returns the first tied target — so callers that need target details
/// (e.g. its drops) don't have to issue a second `find_optimal_gathering` call.
pub(crate) async fn optimize_gather(
    client: &Client,
    name: &'static str,
    character: &mut Character,
    skill: &'static str,
    state: &GameState,
    optimize_equipment: bool,
) -> Result<Option<GatherTarget>> {
    match find_optimal_gathering(client, name, skill, &state.data).await {
        Ok(mut targets) if !targets.is_empty() => {
            let all_locs: Vec<(String, i32, i32)> = targets.iter()
                .flat_map(|t| t.locations.iter().cloned())
                .collect();
            let label = targets.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join("/");
            move_to_nearest(client, name, character, &all_locs, &label).await?;

            if optimize_equipment {
                if let Some(resource) = state.data.resources.iter().find(|r| r.code == targets[0].code) {
                    let role    = ItemRole::Gathering { skill, resource };
                    let ratings = optimize_items(character, skill, resource, &state.data);
                    // Fills in the slots the gathering formula has no opinion on (combat fallback).
                    let ratings = brute_force_optimal_loadout(name, character, &role, &state.data, &ratings);
                    state.set_item_ratings(name, ratings, character);
                    // Same reasoning as optimize_fight: ratings just changed, so a bank item that
                    // didn't qualify before might now — re-check and flag it immediately.
                    state.flag_bank_upgrades_for(name).await;
                }
            }

            Ok(Some(targets.remove(0)))
        }
        Ok(_) => { eprintln!("[{}] No {} sources found at current skill level", crate::ts_char(name), skill); Ok(None) }
        Err(e) => { eprintln!("[{}] Optimization error: {}", crate::ts_char(name), e); Ok(None) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::GameData;
    use crate::types::{CraftIngredient, CraftInfo, Item};

    fn blank_character() -> Character {
        Character {
            name: "test".into(), account: "test".into(), skin: "".into(),
            level: 1, xp: 0, max_xp: 0, gold: 0, speed: 0,
            mining_level: 1, mining_xp: 0, mining_max_xp: 0,
            woodcutting_level: 1, woodcutting_xp: 0, woodcutting_max_xp: 0,
            fishing_level: 1, fishing_xp: 0, fishing_max_xp: 0,
            weaponcrafting_level: 1, weaponcrafting_xp: 0, weaponcrafting_max_xp: 0,
            gearcrafting_level: 1, gearcrafting_xp: 0, gearcrafting_max_xp: 0,
            jewelrycrafting_level: 1, jewelrycrafting_xp: 0, jewelrycrafting_max_xp: 0,
            cooking_level: 1, cooking_xp: 0, cooking_max_xp: 0,
            alchemy_level: 1, alchemy_xp: 0, alchemy_max_xp: 0,
            hp: 100, max_hp: 100, haste: 0, critical_strike: 0, wisdom: 0,
            prospecting: 0, initiative: 0, threat: 0,
            attack_fire: 0, attack_earth: 0, attack_water: 0, attack_air: 0,
            dmg: 0, dmg_fire: 0, dmg_earth: 0, dmg_water: 0, dmg_air: 0,
            res_fire: 0, res_earth: 0, res_water: 0, res_air: 0,
            effects: vec![], x: 0, y: 0, layer: "interior".into(), map_id: 0,
            cooldown: 0, cooldown_expiration: None,
            weapon_slot: "".into(), rune_slot: "".into(), shield_slot: "".into(),
            helmet_slot: "".into(), body_armor_slot: "".into(), leg_armor_slot: "".into(),
            boots_slot: "".into(), ring1_slot: "".into(), ring2_slot: "".into(),
            amulet_slot: "".into(), artifact1_slot: "".into(), artifact2_slot: "".into(),
            artifact3_slot: "".into(), utility1_slot: "".into(), utility1_slot_quantity: 0,
            utility2_slot: "".into(), utility2_slot_quantity: 0, bag_slot: "".into(),
            task: "".into(), task_type: "".into(), task_progress: 0, task_total: 0,
            inventory_max_items: 100, inventory: vec![],
        }
    }

    fn blank_data(items: Vec<Item>) -> GameData {
        GameData { monsters: vec![], items, resources: vec![], maps: vec![], craftable_equip: vec![] }
    }

    fn make_craft_item(code: &str, ingredients: Vec<(&str, i32)>) -> Item {
        Item {
            name: code.into(), code: code.into(), level: 1, item_type: "weapon".into(),
            subtype: "".into(), description: "".into(), conditions: vec![], effects: vec![],
            craft: Some(CraftInfo {
                skill: Some("weaponcrafting".into()),
                level: None,
                items: ingredients.into_iter().map(|(c, q)| CraftIngredient { code: c.into(), quantity: q }).collect(),
                quantity: 1,
            }),
            tradeable: true, recyclable: false,
        }
    }

    #[test]
    fn should_farm_wishlist_drops_uses_global_subtier_not_raw_level() {
        let state = GameState::new(blank_data(vec![]));
        state.set_crafter_min_gear_level(10); // global_subtier(10) = 2

        let mut character = blank_character();
        character.level = 14; // global_subtier(14) = 2 -- same subtier, not strictly past it
        assert!(!should_farm_wishlist_drops(&character, &state));

        character.level = 15; // global_subtier(15) = 3 -- strictly past
        assert!(should_farm_wishlist_drops(&character, &state));
    }

    #[test]
    fn wishlist_material_codes_resolves_direct_ingredients_only() {
        let data = blank_data(vec![make_craft_item("iron_sword", vec![("iron_bar", 2), ("wood", 1)])]);
        let state = GameState::new(data);

        let codes = wishlist_material_codes(&state, &["iron_sword".to_string()]);

        assert_eq!(codes, ["iron_bar".to_string(), "wood".to_string()].into_iter().collect());
    }

    #[test]
    fn wishlist_material_codes_ignores_unknown_item_codes() {
        let state = GameState::new(blank_data(vec![]));
        let codes = wishlist_material_codes(&state, &["does_not_exist".to_string()]);
        assert!(codes.is_empty());
    }
}
