use std::sync::Arc;

use crate::api::{build_client, deposit_to_bank, gather_material, get_character, is_inventory_full, wait_for_cooldown};
use crate::flags::GameState;
use crate::optimize::find_optimal_gathering;
use crate::types::Result;

use super::bank_ops::deposit_all;
use super::crafting_exec::run_crafting_phase;
use super::crafting_plan::refresh_shared_crafting_state;
use super::fight::run_fight_mode;
use super::flag_handling::handle_flags;
use super::movement::move_to_nearest;
use super::promotion::should_promote;
use super::repositioning::optimize_gather;
use super::{on_skill_level_up, CharacterRole, RESTART_DELAY};

/// The alchemy-crafting character can be promoted into fighting once its own alchemy level
/// outpaces the dedicated fighter's (see `loops::promotion`): this is a small dispatcher that
/// tracks which mode it's currently in and calls either `alchemy_and_crafting_loop_body` or
/// `run_fight_mode`. While fighting, it keeps crafting reactively — see `run_fight_mode`'s
/// `handle_flags` call, which drains `FlagAction::CraftWishlistGearAvailable` and runs a normal
/// crafting phase without leaving fighting mode. Both loop bodies return `Ok(())` specifically to
/// signal "switch me to the other mode" (see `run_fight_mode`'s doc comment) — an `Err` is a
/// genuine action failure, logged and retried in the *same* mode instead.
pub async fn alchemy_and_crafting_loop(name: &'static str, state: Arc<GameState>) {
    let mut fighting = false;
    loop {
        let result = if fighting {
            run_fight_mode(name, &state, Some("alchemy"), true).await
        } else {
            alchemy_and_crafting_loop_body(name, &state).await
        };

        match result {
            Ok(()) => fighting = !fighting,
            Err(e) => {
                eprintln!("[{}] Action failed after all retries — restarting loop: {}", crate::ts_char(name), e);
                tokio::time::sleep(RESTART_DELAY).await;
            }
        }
    }
}

/// Gathers alchemy resources, then crafts alongside the weaponcrafting/gearcrafting/
/// jewelrycrafting phase (`run_crafting_phase`) — alchemy is now just another skill in that
/// phase's filler pool, ranked by XP per ingredient like the others. `filler_candidates` walks
/// weaponcrafting/gearcrafting/jewelrycrafting before alchemy, so an ingredient shared between an
/// equipment recipe and an alchemy recipe always loses to the equipment recipe. Returns `Ok(())`
/// early — before doing anything else — the moment alchemy reaches the fighting threshold (see
/// `alchemy_and_crafting_loop`), whether that's true from the very first character fetch or only
/// becomes true after a level-up partway through.
async fn alchemy_and_crafting_loop_body(name: &'static str, state: &GameState) -> Result<()> {
    let client = build_client();

    let mut character = get_character(&client, name).await?;
    refresh_shared_crafting_state(state, &character);

    if should_promote(&character, "alchemy", state.fighter_combat_level()) {
        println!("[{}] alchemy level has reached the fighting threshold — switching to combat.", crate::ts_char(name));
        return Ok(());
    }

    // Item ratings for this character were already computed during program initialization;
    // just find the target.
    let mut alchemy_target = match find_optimal_gathering(&client, name, "alchemy", &state.data).await {
        Ok(mut t) if !t.is_empty() => t.remove(0),
        Ok(_)  => return Err("no alchemy sources found at current level".into()),
        Err(e) => return Err(e),
    };

    if is_inventory_full(&character) {
        character = deposit_to_bank(&client, name, &character, state).await?;
    }

    move_to_nearest(&client, name, &mut character, &alchemy_target.locations, &alchemy_target.name).await?;

    let mut prev_alchemy_level: Option<i32> = None;

    loop {
        let max_drops_per_action: i32 = alchemy_target.drops.iter().map(|d| d.max_quantity).sum();

        // Phase 1: Gather until one action away from full
        loop {
            let total_items: i32 = character.inventory.iter().map(|s| s.quantity).sum();
            if character.inventory_max_items - total_items <= max_drops_per_action {
                break;
            }
            let result = gather_material(&client, name).await?;
            let level = result.character.alchemy_level;
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;

            if let Some(prev) = prev_alchemy_level {
                if level > prev {
                    on_skill_level_up(name, "alchemy", level);
                    // A skill level-up can unlock better gear (level conditions) or shift the
                    // whole XP/hour landscape — re-run the full brute-force optimization, check
                    // the bank against the fresh ratings, and flag any upgrade found there.
                    if let Some(new_target) = optimize_gather(&client, name, &mut character, "alchemy", state, true).await? {
                        alchemy_target = new_target;
                    }
                }
            }
            prev_alchemy_level = Some(level);

            if should_promote(&character, "alchemy", state.fighter_combat_level()) {
                println!("[{}] alchemy level {} has reached the fighting threshold — switching to combat.", crate::ts_char(name), level);
                return Ok(());
            }
        }

        // Phase 2: Deposit everything at the bank
        character = deposit_all(&client, name, character, state).await?;

        // Phase 3: Weaponcrafting/gearcrafting/jewelrycrafting/alchemy until bank materials are exhausted
        character = run_crafting_phase(&client, name, character, state).await?;
        refresh_shared_crafting_state(state, &character);

        // Phase 4: Handle flags, re-evaluate optimal alchemy target, and move back to it
        // (covers alchemy level-ups that change the best resource).
        handle_flags(state, name, &client, &mut character, CharacterRole::Gathering("alchemy")).await?;

        if let Ok(mut targets) = find_optimal_gathering(&client, name, "alchemy", &state.data).await {
            if !targets.is_empty() {
                alchemy_target = targets.remove(0);
            }
        }

        move_to_nearest(&client, name, &mut character, &alchemy_target.locations, &alchemy_target.name).await?;
    }
}
