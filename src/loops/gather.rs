use std::sync::Arc;

use crate::api::{build_client, gather_material, get_character, is_inventory_full, wait_for_cooldown};
use crate::flags::GameState;
use crate::optimize::{find_optimal_gathering, skill_level};
use crate::types::Result;

use super::flag_handling::handle_flags;
use super::movement::move_to_nearest;
use super::refining::run_refining_phase;
use super::{on_skill_level_up, CharacterRole, RESTART_DELAY};

/// Woodcutting, mining, and fishing all follow the exact same process: gather until one action
/// away from a full inventory, then refine (via `run_refining_phase`) the highest-level recipe(s)
/// available for the skill's refined material — woodcutting/mining refine into their own skill
/// (planks/bars/gems), fishing refines via cooking — starting from whatever's already in hand
/// (no bank trip needed if that alone covers it) and topping up from the bank only as needed,
/// making as many bank/workstation trips as necessary until only reserved raw materials remain.
/// Then re-evaluate the optimal gathering spot (covers level-ups) and repeat.
pub async fn gather_loop(name: &'static str, skill: &'static str, state: Arc<GameState>) {
    loop {
        if let Err(e) = gather_loop_body(name, skill, &state).await {
            eprintln!("[{}] Action failed after all retries — restarting loop: {}", crate::ts_char(name), e);
            tokio::time::sleep(RESTART_DELAY).await;
        }
    }
}

async fn gather_loop_body(name: &'static str, skill: &'static str, state: &GameState) -> Result<()> {
    let client = build_client();

    let mut character = get_character(&client, name).await?;

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
            if let Some(prev) = prev_skill_level {
                if level > prev {
                    on_skill_level_up(name, skill, level);
                }
            }
            prev_skill_level = Some(level);
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;
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
