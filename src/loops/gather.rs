use std::sync::Arc;

use crate::api::{build_client, gather_material, get_character, is_inventory_full, wait_for_cooldown};
use crate::flags::GameState;
use crate::optimize::{find_optimal_gathering, skill_level};
use crate::types::Result;

use super::fight::run_fight_mode;
use super::flag_handling::handle_flags;
use super::movement::move_to_nearest;
use super::promotion::should_promote;
use super::refining::run_refining_phase;
use super::repositioning::optimize_gather;
use super::{on_skill_level_up, CharacterRole, RESTART_DELAY};

/// Woodcutting, mining, and fishing all follow the exact same process, and can each be promoted
/// into fighting once their own level outpaces the dedicated fighter's (see `loops::promotion`):
/// this is a small dispatcher that tracks which mode it's currently in and calls either
/// `gather_loop_body` or `run_fight_mode`. Both return `Ok(())` specifically to signal "switch me
/// to the other mode" (see `run_fight_mode`'s doc comment) — an `Err` is a genuine action failure,
/// logged and retried in the *same* mode instead.
pub async fn gather_loop(name: &'static str, skill: &'static str, state: Arc<GameState>) {
    let mut fighting = false;
    loop {
        let result = if fighting {
            run_fight_mode(name, &state, Some(skill), true).await
        } else {
            gather_loop_body(name, skill, &state).await
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

/// Gather until one action away from a full inventory, then refine (via `run_refining_phase`) the
/// highest-level recipe(s) available for the skill's refined material — woodcutting/mining refine
/// into their own skill (planks/bars/gems), fishing refines via cooking — starting from whatever's
/// already in hand (no bank trip needed if that alone covers it) and topping up from the bank only
/// as needed, making as many bank/workstation trips as necessary until only reserved raw materials
/// remain. Then re-evaluate the optimal gathering spot (covers level-ups) and repeat. Returns
/// `Ok(())` early — before doing anything else — the moment `skill` reaches the fighting threshold
/// (see `gather_loop`), whether that's true from the very first character fetch or only becomes
/// true after a level-up partway through.
async fn gather_loop_body(name: &'static str, skill: &'static str, state: &GameState) -> Result<()> {
    let client = build_client();

    let mut character = get_character(&client, name).await?;

    if should_promote(&character, skill, state.fighter_combat_level()) {
        println!("[{}] {} level has reached the fighting threshold — switching to combat.", crate::ts_char(name), skill);
        return Ok(());
    }

    // Item ratings for this character were already computed during program initialization;
    // just find the target.
    let mut target = match find_optimal_gathering(&client, name, skill, &state.data).await {
        Ok(mut t) if !t.is_empty() => t.remove(0),
        Ok(_)  => return Err(format!("no {} sources found at current level", skill).into()),
        Err(e) => return Err(e),
    };

    if is_inventory_full(&character) {
        character = run_refining_phase(&client, name, character, state, skill).await?;
    }

    move_to_nearest(&client, name, &mut character, &target.locations, &target.name).await?;

    let mut prev_skill_level: Option<i32> = None;

    loop {
        let max_drops_per_action: i32 = target.drops.iter().map(|d| d.max_quantity).sum();

        // Phase 1: Gather until one action away from full
        loop {
            let total_items: i32 = character.inventory.iter().map(|s| s.quantity).sum();
            if character.inventory_max_items - total_items <= max_drops_per_action {
                break;
            }
            let result = gather_material(&client, name).await?;
            let level = skill_level(&result.character, skill);
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;

            if let Some(prev) = prev_skill_level {
                if level > prev {
                    on_skill_level_up(name, skill, level);
                    // A skill level-up can unlock better gear (level conditions) or shift the
                    // whole XP/hour landscape — re-run the full brute-force optimization, check
                    // the bank against the fresh ratings, and flag any upgrade found there.
                    if let Some(new_target) = optimize_gather(&client, name, &mut character, skill, state, true).await? {
                        target = new_target;
                    }
                }
            }
            prev_skill_level = Some(level);

            if should_promote(&character, skill, state.fighter_combat_level()) {
                println!("[{}] {} level {} has reached the fighting threshold — switching to combat.", crate::ts_char(name), skill, level);
                return Ok(());
            }
        }

        // Phase 2: Refine what's already in hand first (no bank trip if that's all it needs),
        // then top up from the bank as needed — until only reserved raw materials remain.
        character = run_refining_phase(&client, name, character, state, skill).await?;

        // Phase 3: Handle flags, re-evaluate optimal target, move back.
        handle_flags(state, name, &client, &mut character, CharacterRole::Gathering(skill)).await?;

        if let Ok(mut targets) = find_optimal_gathering(&client, name, skill, &state.data).await {
            if !targets.is_empty() {
                target = targets.remove(0);
            }
        }

        move_to_nearest(&client, name, &mut character, &target.locations, &target.name).await?;
    }
}
