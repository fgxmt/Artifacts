use reqwest::Client;

use crate::flags::GameState;
use crate::optimize::{
    brute_force_optimal_loadout, find_optimal_gathering, find_optimal_monster,
    optimize_combat_loadout, optimize_items, GatherTarget, ItemRole,
};
use crate::types::{Character, Result};

use super::movement::move_to_nearest;

pub(crate) async fn optimize_fight(
    client: &Client,
    name: &'static str,
    character: &mut Character,
    state: &GameState,
    optimize_equipment: bool,
) -> Result<Option<i32>> {
    match find_optimal_monster(client, name, &state.data).await {
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
                }
            }

            Ok(Some(targets.remove(0)))
        }
        Ok(_) => { eprintln!("[{}] No {} sources found at current skill level", crate::ts_char(name), skill); Ok(None) }
        Err(e) => { eprintln!("[{}] Optimization error: {}", crate::ts_char(name), e); Ok(None) }
    }
}
