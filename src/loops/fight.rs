use std::sync::Arc;

use crate::api::{build_client, deposit_to_bank, fight_monster, get_character, is_inventory_full, rest_character, wait_for_cooldown};
use crate::flags::GameState;
use crate::types::Result;

use super::flag_handling::handle_flags;
use super::repositioning::optimize_fight;
use super::{on_combat_level_up, CharacterRole, RESTART_DELAY};

pub async fn fight_loop(name: &'static str, state: Arc<GameState>) {
    loop {
        if let Err(e) = fight_loop_body(name, &state).await {
            eprintln!("[{}] Action failed after all retries — restarting loop: {}", crate::ts_char(name), e);
            tokio::time::sleep(RESTART_DELAY).await;
        }
    }
}

async fn fight_loop_body(name: &'static str, state: &GameState) -> Result<()> {
    let client = build_client();

    let mut initial_char = get_character(&client, name).await?;

    if is_inventory_full(&initial_char) {
        initial_char = deposit_to_bank(&client, name, &initial_char, state).await?;
    }

    // Item ratings for this character were already computed during program initialization;
    // just find the target and move there.
    let mut min_hp_threshold = optimize_fight(&client, name, &mut initial_char, state, false).await?.unwrap_or(0);
    let mut prev_level: Option<i32> = None;

    if initial_char.hp < min_hp_threshold {
        println!(
            "[{}] HP ({}) below threshold ({}). Resting.",
            crate::ts_char(name), initial_char.hp, min_hp_threshold
        );
        let rest = rest_character(&client, name).await?;
        wait_for_cooldown(&rest.cooldown).await;
    }

    loop {
        let result = fight_monster(&client, name).await?;
        let mut character = result.character;
        let level = character.level;

        if let Some(prev) = prev_level {
            if level > prev {
                on_combat_level_up(name, level);
                if let Some(threshold) = optimize_fight(&client, name, &mut character, state, true).await? {
                    min_hp_threshold = threshold;
                }
            }
        }
        prev_level = Some(level);

        wait_for_cooldown(&result.cooldown).await;

        if is_inventory_full(&character) {
            character = deposit_to_bank(&client, name, &character, state).await?;
        }

        if character.hp < min_hp_threshold {
            println!(
                "[{}] HP ({}) below threshold ({}). Resting.",
                crate::ts_char(name), character.hp, min_hp_threshold
            );
            let rest = rest_character(&client, name).await?;
            wait_for_cooldown(&rest.cooldown).await;
            character = rest.character;
        }

        handle_flags(state, name, &client, &mut character, CharacterRole::Combat).await?;
    }
}
