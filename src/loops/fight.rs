use std::sync::Arc;

use crate::api::{build_client, deposit_to_bank, fight_monster, get_character, is_inventory_full, wait_for_cooldown};
use crate::flags::GameState;
use crate::formulas::gather_promotion_threshold;
use crate::optimize::skill_level;
use crate::types::{Character, Result};

use super::consumables::{ensure_healing_consumables, heal_to_threshold};
use super::flag_handling::handle_flags;
use super::repositioning::optimize_fight;
use super::utility::{ensure_utility_loadout, top_up_utility_stock};
use super::{on_combat_level_up, CharacterRole, RESTART_DELAY};

/// Whether a utility slot that held charges a moment ago (`before`) has since run dry (`after`) —
/// the trigger for restocking and, since losing an auto-heal changes worst-case survivability,
/// for re-running `optimize_fight` even when nothing was available in the bank to replace it.
fn utility_ran_dry(before: &Character, after: &Character) -> bool {
    (before.utility1_slot_quantity > 0 && after.utility1_slot_quantity <= 0)
        || (before.utility2_slot_quantity > 0 && after.utility2_slot_quantity <= 0)
}

/// Opportunistic top-up: called whenever the character has just visited (or is at) the bank for
/// some other reason. A no-op if every stocked utility slot is already full. Re-runs the cheap
/// monster/threshold recalc only when something actually changed.
async fn refresh_utility_stock(
    client: &reqwest::Client,
    name: &'static str,
    character: Character,
    state: &GameState,
    min_hp_threshold: &mut i32,
) -> Result<Character> {
    let (mut character, topped_up) = top_up_utility_stock(client, name, character, state).await?;
    if topped_up {
        if let Some(threshold) = optimize_fight(client, name, &mut character, state, false).await? {
            *min_hp_threshold = threshold;
        }
    }
    Ok(character)
}

pub async fn fight_loop(name: &'static str, state: Arc<GameState>) {
    loop {
        if let Err(e) = run_fight_mode(name, &state, None, false).await {
            eprintln!("[{}] Action failed after all retries — restarting loop: {}", crate::ts_char(name), e);
            tokio::time::sleep(RESTART_DELAY).await;
        }
    }
}

/// Runs combat indefinitely — the full fight-loop behavior (utility/healing-consumable
/// management, level-up reoptimization, flag handling, wishlist-drop-vs-XP/hour targeting) —
/// until either an action fails after every retry (`Err`, the caller's dispatcher should log and
/// retry the same mode), or, if `revert_skill` is `Some`, this character's own level in that skill
/// has fallen back below the fighting threshold relative to the dedicated fighter's current combat
/// level (a clean `Ok(())`, signaling the caller's dispatcher to switch back to that skill's
/// gathering/crafting loop — see `loops::promotion`).
///
/// `revert_skill` is `None` for the dedicated fighting character itself, which never reverts and
/// is also the only character whose combat level feeds `GameState::fighter_combat_level` (the
/// reference every other character's promotion threshold is computed against — a promoted
/// character's own combat XP has no bearing on anyone else's threshold). `force_reoptimize_on_entry`
/// re-runs the full combat gear brute-force before the first fight, needed for a character just
/// promoted into fighting whose cached ratings are still gathering-oriented; the dedicated fighter
/// passes `false` since its ratings are already combat-oriented from program init.
pub(crate) async fn run_fight_mode(
    name: &'static str,
    state: &GameState,
    revert_skill: Option<&'static str>,
    force_reoptimize_on_entry: bool,
) -> Result<()> {
    let client = build_client();

    let mut character = get_character(&client, name).await?;

    if is_inventory_full(&character) {
        character = deposit_to_bank(&client, name, &character, state).await?;
    }
    let (character, _) = ensure_utility_loadout(&client, name, character, state).await?;
    let (character, _) = top_up_utility_stock(&client, name, character, state).await?;
    let mut character = ensure_healing_consumables(&client, name, character, state).await?;

    // Item ratings for this character were already computed during program initialization (or,
    // for a just-promoted character, `force_reoptimize_on_entry` forces a fresh combat-oriented
    // pass here); just find the target and move there.
    let mut min_hp_threshold = optimize_fight(&client, name, &mut character, state, force_reoptimize_on_entry).await?.unwrap_or(0);
    let mut prev_level: Option<i32> = None;

    character = heal_to_threshold(&client, name, character, min_hp_threshold, state).await?;

    loop {
        if let Some(skill) = revert_skill {
            if skill_level(&character, skill) < gather_promotion_threshold(state.fighter_combat_level()) {
                println!(
                    "[{}] {} level has fallen back below the fighting threshold — resuming {}.",
                    crate::ts_char(name), skill, skill
                );
                return Ok(());
            }
        }

        let before_fight = character.clone();

        let result = fight_monster(&client, name).await?;
        character = result.character;
        let level = character.level;

        if let Some(prev) = prev_level {
            if level > prev {
                on_combat_level_up(name, level);
                if revert_skill.is_none() {
                    // Only the dedicated fighter's own combat level is the reference other
                    // characters' promotion thresholds are computed against.
                    state.set_fighter_combat_level(level);
                }
                if let Some(threshold) = optimize_fight(&client, name, &mut character, state, true).await? {
                    min_hp_threshold = threshold;
                }
            }
        }
        prev_level = Some(level);

        wait_for_cooldown(&result.cooldown).await;

        // Primary bank trigger: out of healing consumables (not raw inventory fullness) — a no-op
        // if some are still held, and only actually visits the bank if it has a replacement (see
        // `ensure_healing_consumables`; otherwise the character keeps fighting and waits for a
        // flag once the bank is restocked by someone else).
        character = ensure_healing_consumables(&client, name, character, state).await?;

        // Fallback: inventory can still fill up with loot even while healing consumables are held
        // (they can't be used at full/near-full HP), so this stays as a safety net.
        if is_inventory_full(&character) {
            character = deposit_to_bank(&client, name, &character, state).await?;
            character = refresh_utility_stock(&client, name, character, state, &mut min_hp_threshold).await?;
            character = ensure_healing_consumables(&client, name, character, state).await?;
        }

        // A utility slot ran dry this fight — restock it if the bank has a replacement, and
        // either way re-run the (cheap) monster/threshold recalc: losing or regaining an auto-heal
        // changes worst-case survivability even when the equipped gear hasn't changed at all.
        if utility_ran_dry(&before_fight, &character) {
            let (updated, _restocked) = ensure_utility_loadout(&client, name, character, state).await?;
            character = updated;
            if let Some(threshold) = optimize_fight(&client, name, &mut character, state, false).await? {
                min_hp_threshold = threshold;
            }
            // While there, also top up the other utility slot if it isn't full.
            character = refresh_utility_stock(&client, name, character, state, &mut min_hp_threshold).await?;
        }

        // Eat healing consumables up to the survivability threshold; only falls back to resting
        // if none are on hand at all.
        character = heal_to_threshold(&client, name, character, min_hp_threshold, state).await?;

        handle_flags(state, name, &client, &mut character, CharacterRole::Combat).await?;

        // handle_flags may have made its own bank trip (equip-upgrade, healing-consumable-restock,
        // or craft-wishlist-gear flags) — top up utility stock while there too.
        character = refresh_utility_stock(&client, name, character, state, &mut min_hp_threshold).await?;
    }
}
