use reqwest::Client;

use crate::api::get_character;
use crate::flags::GameState;
use crate::optimize::{
    brute_force_optimal_loadout, find_optimal_gathering, optimize_combat_loadout, optimize_items,
    ItemRole,
};
use crate::types::{Character, Result};

use super::flag_handling::handle_flags;
use super::CharacterRole;

/// Fetches `name`'s character and computes + caches their item ratings against their optimal
/// target (combat monster, or gathering resource for `role_skill`) — without moving them.
/// Meant for program startup, so `allocate_upgrade_crafts` reflects real equip-upgrade needs from the
/// first crafting plan onward instead of starting empty. `role_skill` is `None` for the combat
/// character, `Some(gathering_skill)` otherwise. Only a failed `get_character` is fatal here;
/// a failure to find an optimal target just skips caching ratings for this character (they'll
/// get them once their own loop starts).
pub async fn init_character_ratings(
    client: &Client,
    name: &'static str,
    role_skill: Option<&'static str>,
    state: &GameState,
) -> Result<Character> {
    let mut character = get_character(client, name).await?;

    match role_skill {
        None => {
            // Full joint brute-force search over all 13 slots — see optimize.rs for why this
            // can't just be an unrestricted exhaustive search. Blocks until done before this
            // function returns, so main.rs's sequential per-character init loop won't start the
            // next character early. Doesn't need a target monster up front: discovering which
            // monster (if any) ends up best-achievable is part of what the search does, so gating
            // it on a monster already being winnable with *current* gear would rule out exactly
            // the case where better gear is what makes the first win possible.
            let ratings = optimize_combat_loadout(name, &character, &state.data);
            state.set_item_ratings(name, ratings, &character);
        },
        Some(skill) => match find_optimal_gathering(client, name, skill, &state.data).await {
            Ok(mut targets) if !targets.is_empty() => {
                let target = targets.remove(0);
                if let Some(resource) = state.data.resources.iter().find(|r| r.code == target.code) {
                    let role    = ItemRole::Gathering { skill, resource };
                    let ratings = optimize_items(&character, skill, resource, &state.data);
                    // Combat-fallback slots only (see optimize.rs) — the gathering-relevant
                    // slots are already globally optimal from independent per-slot analysis.
                    let ratings = brute_force_optimal_loadout(name, &character, &role, &state.data, &ratings);
                    state.set_item_ratings(name, ratings, &character);
                }
            }
            Ok(_) => eprintln!("[{}] No {} sources found at current skill level", crate::ts_char(name), skill),
            Err(e) => eprintln!("[{}] Optimization error: {}", crate::ts_char(name), e),
        },
    }

    // The bank was loaded — and its one-time upgrade scan run — before any character had ratings
    // cached, so an upgrade already sitting in the bank at startup was never flagged for anyone.
    // Now that this character's ratings exist, re-check the bank against them and, if anything
    // turns up, go equip it right away via the same flag-handling path the main loop uses —
    // rather than leaving it to sit unequipped until some future deposit happens to re-trigger
    // the check.
    state.flag_bank_upgrades_for(name).await;
    let role = match role_skill {
        None => CharacterRole::Combat,
        Some(skill) => CharacterRole::Gathering(skill),
    };
    handle_flags(state, name, client, &mut character, role).await?;

    Ok(character)
}
