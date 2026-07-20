use std::collections::HashSet;
use std::sync::Arc;

use crate::api::{build_client, craft_item, deposit_items, deposit_to_bank, gather_material, get_bank_items, get_character, is_inventory_full, move_character, wait_for_cooldown};
use crate::flags::GameState;
use crate::formulas::craftable_quantity;
use crate::optimize::{find_optimal_gathering, locations_raw_for};
use crate::types::{DepositItem, Result};

use super::crafting_exec::run_crafting_phase;
use super::flag_handling::handle_flags;
use super::movement::move_to_nearest;
use super::{on_skill_level_up, CharacterRole, RESTART_DELAY};

pub async fn alchemy_and_crafting_loop(name: &'static str, state: Arc<GameState>) {
    loop {
        if let Err(e) = alchemy_and_crafting_loop_body(name, &state).await {
            eprintln!("[{}] Action failed after all retries — restarting loop: {}", crate::ts_char(name), e);
            tokio::time::sleep(RESTART_DELAY).await;
        }
    }
}

async fn alchemy_and_crafting_loop_body(name: &'static str, state: &GameState) -> Result<()> {
    let client = build_client();

    let mut character = get_character(&client, name).await?;

    // Item ratings for this character were already computed during program initialization;
    // just find the target.
    let mut alchemy_target = match find_optimal_gathering(&client, name, "alchemy", &state.data).await {
        Ok(mut t) if !t.is_empty() => t.remove(0),
        Ok(_)  => return Err("no alchemy sources found at current level".into()),
        Err(e) => return Err(e),
    };

    let alchemy_workshop_locs = locations_raw_for(&state.data.maps, "workshop", "alchemy");

    if is_inventory_full(&character) {
        character = deposit_to_bank(&client, name, &character, state).await?;
    }

    move_to_nearest(&client, name, &mut character, &alchemy_target.locations, &alchemy_target.name).await?;

    let mut prev_alchemy_level: Option<i32> = None;

    loop {
        let resource_drop_codes: HashSet<String> = alchemy_target.drops.iter().map(|d| d.code.clone()).collect();
        let max_drops_per_action: i32 = alchemy_target.drops.iter().map(|d| d.max_quantity).sum();

        let alchemy_recipes: Vec<_> = state.data.items.iter().filter(|item| {
            item.craft.as_ref().is_some_and(|c| {
                c.skill.as_deref() == Some("alchemy") &&
                c.items.iter().any(|ing| resource_drop_codes.contains(&ing.code))
            })
        }).cloned().collect();

        // Phase 1: Gather until one action away from full
        loop {
            let total_items: i32 = character.inventory.iter().map(|s| s.quantity).sum();
            if character.inventory_max_items - total_items <= max_drops_per_action {
                break;
            }
            let result = gather_material(&client, name).await?;
            let level = result.character.alchemy_level;
            if let Some(prev) = prev_alchemy_level {
                if level > prev {
                    on_skill_level_up(name, "alchemy", level);
                }
            }
            prev_alchemy_level = Some(level);
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;
        }

        // Phase 2: Craft alchemy items at workstation
        if !alchemy_recipes.is_empty() && !alchemy_workshop_locs.is_empty() {
            move_to_nearest(&client, name, &mut character, &alchemy_workshop_locs, "Alchemy workstation").await?;

            for recipe in &alchemy_recipes {
                if let Some(craft) = &recipe.craft {
                    let qty = craftable_quantity(&character.inventory, craft);
                    if qty > 0 {
                        let result = craft_item(&client, name, &recipe.code, qty).await?;
                        wait_for_cooldown(&result.cooldown).await;
                        character = result.character;
                    }
                }
            }
        }

        // Phase 3: Deposit everything at the bank
        let items_to_deposit: Vec<DepositItem> = character.inventory.iter()
            .filter(|s| s.quantity > 0 && !s.code.is_empty())
            .map(|s| DepositItem { code: s.code.clone(), quantity: s.quantity })
            .collect();

        if !items_to_deposit.is_empty() {
            let result = move_character(&client, name, 4, 1).await?;
            wait_for_cooldown(&result.cooldown).await;

            println!("[{}] Depositing {} item stack(s) to bank...", crate::ts_char(name), items_to_deposit.len());
            let result = deposit_items(&client, name, items_to_deposit).await?;
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;

            let bank = get_bank_items(&client).await?;
            let total: i32 = bank.iter().map(|b| b.quantity).sum();
            println!("[{}] Bank updated: {} stacks ({} total items)", crate::ts_char(name), bank.len(), total);
            state.update_bank(bank).await;
        }

        // Phase 4: Weaponcrafting/gearcrafting/jewelrycrafting until bank materials are exhausted
        character = run_crafting_phase(&client, name, character, state).await?;

        // Phase 5: Handle flags, re-evaluate optimal alchemy target, and move back to it
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
