use std::collections::{HashMap, HashSet};

use reqwest::Client;

use crate::api::{craft_item, deposit_gold, deposit_items, get_bank_items, move_character, wait_for_cooldown, withdraw_items};
use crate::flags::GameState;
use crate::optimize::locations_raw_for;
use crate::types::{Character, DepositItem, Result};

use super::bank_ops::print_plan;
use super::crafting_plan::{filler_candidates, plan_crafting_priority, PlanTier};
use super::movement::move_to_nearest;

/// A finalized craft (after trimming for inventory/slot budget) ready to withdraw and craft.
struct FinalizedCraft {
    code: String,
    skill: String,
    quantity: i32,
}

/// Withdraws materials for the current craft plan and crafts them, respecting the inventory's
/// 20-slot / `inventory_max_items` budget while reserving one slot per distinct item type
/// being crafted (for its output stack) and at least one free space overall. Deposits
/// everything crafted back to the bank, then repeats with a freshly-computed plan until the
/// bank has no material left for even the cheapest planned craft.
pub(crate) async fn run_crafting_phase(
    client: &Client,
    name: &'static str,
    mut character: Character,
    state: &GameState,
) -> Result<Character> {
    loop {
        let bank = state.bank_snapshot().await;

        // Remaining bank supply per ingredient code, decremented as each candidate is
        // committed — without this, two candidates sharing an ingredient (e.g. copper boots and
        // copper axes both needing copper bars) would each be sized against the *full* bank
        // total, together planning more than the bank actually has.
        let mut remaining_bank: HashMap<String, i32> = HashMap::new();
        for b in &bank {
            *remaining_bank.entry(b.code.clone()).or_insert(0) += b.quantity;
        }

        // Only entries that CAN'T be fully made right now (missing ingredients or crafting
        // level) get reserved; anything fully suppliable is planned below instead — it isn't
        // being "held back", it's about to be consumed. Upgrades claim materials before wishlist
        // entries do (see plan_crafting_priority).
        let (mut candidates, reserved, exclude) = plan_crafting_priority(state, &character, &mut remaining_bank);
        candidates.extend(filler_candidates(state, &character, &exclude));

        if candidates.is_empty() {
            println!("[{}] Nothing left to craft — heading back to gathering.", crate::ts_char(name));
            break;
        }

        let max_slots = 20;
        let max_qty   = (character.inventory_max_items - 1).max(0);

        let mut used_slots: HashSet<String> = HashSet::new();
        let mut used_qty = 0_i32;
        let mut withdraw_list: Vec<DepositItem> = Vec::new();
        let mut final_plan: Vec<FinalizedCraft> = Vec::new();

        for entry in &candidates {
            let item  = match state.data.items.iter().find(|i| i.code == entry.code) { Some(i) => i, None => continue };
            let craft = match &item.craft { Some(c) => c, None => continue };

            // Upgrade and wishlist entries were already confirmed suppliable (and reserved out of
            // remaining_bank) by plan_crafting_priority; fillers still need their own bank check.
            let mut qty = if entry.tier != PlanTier::Filler {
                entry.quantity
            } else {
                craft.items.iter()
                    .map(|ing| remaining_bank.get(&ing.code).copied().unwrap_or(0) / ing.quantity.max(1))
                    .min().unwrap_or(0)
            };

            while qty > 0 {
                // A slot for the crafted output itself, plus one per not-yet-used ingredient.
                let mut extra_slots = if used_slots.contains(&entry.code) { 0 } else { 1 };
                let mut extra_qty   = 0;
                for ing in &craft.items {
                    if !used_slots.contains(&ing.code) { extra_slots += 1; }
                    extra_qty += ing.quantity * qty;
                }
                if used_slots.len() as i32 + extra_slots <= max_slots && used_qty + extra_qty <= max_qty {
                    break;
                }
                qty -= 1;
            }
            if qty <= 0 { continue; }

            used_slots.insert(entry.code.clone());
            for ing in &craft.items {
                used_slots.insert(ing.code.clone());
                let need = ing.quantity * qty;
                used_qty += need;
                if entry.tier == PlanTier::Filler {
                    *remaining_bank.entry(ing.code.clone()).or_insert(0) -= need;
                }
                match withdraw_list.iter_mut().find(|w| w.code == ing.code) {
                    Some(w) => w.quantity += need,
                    None => withdraw_list.push(DepositItem { code: ing.code.clone(), quantity: need }),
                }
            }
            final_plan.push(FinalizedCraft {
                code: entry.code.clone(),
                skill: entry.skill.clone(),
                quantity: qty,
            });
        }

        if withdraw_list.is_empty() {
            println!("[{}] Not enough bank materials/inventory space to craft anything more.", crate::ts_char(name));
            break;
        }

        let planned: Vec<(&str, i32)> = final_plan.iter().map(|p| (p.code.as_str(), p.quantity)).collect();
        print_plan(name, &planned, &reserved);

        // Move to bank & withdraw ingredients for this batch.
        if character.x != 4 || character.y != 1 {
            let result = move_character(client, name, 4, 1).await?;
            wait_for_cooldown(&result.cooldown).await;
        }
        println!("[{}] Withdrawing {} ingredient stack(s) for crafting...", crate::ts_char(name), withdraw_list.len());
        let result = withdraw_items(client, name, withdraw_list).await?;
        wait_for_cooldown(&result.cooldown).await;
        character = result.character;
        if let Ok(bank) = get_bank_items(client).await { state.update_bank(bank).await; }

        // Craft each planned item at its skill's workstation.
        for planned in &final_plan {
            let locs = locations_raw_for(&state.data.maps, "workshop", &planned.skill);
            move_to_nearest(client, name, &mut character, &locs, &planned.skill).await?;

            let result = craft_item(client, name, &planned.code, planned.quantity).await?;
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;
        }

        // Deposit everything crafted before planning the next batch.
        let items_to_deposit: Vec<DepositItem> = character.inventory.iter()
            .filter(|s| s.quantity > 0 && !s.code.is_empty())
            .map(|s| DepositItem { code: s.code.clone(), quantity: s.quantity })
            .collect();

        if character.x != 4 || character.y != 1 {
            let result = move_character(client, name, 4, 1).await?;
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;
        }
        if !items_to_deposit.is_empty() {
            println!("[{}] Depositing {} crafted item stack(s) to bank...", crate::ts_char(name), items_to_deposit.len());
            let result = deposit_items(client, name, items_to_deposit).await?;
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;
            if let Ok(bank) = get_bank_items(client).await { state.update_bank(bank).await; }
        }

        if character.gold > 0 {
            println!("[{}] Depositing {} gold to bank...", crate::ts_char(name), character.gold);
            let result = deposit_gold(client, name, character.gold).await?;
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;
        }
    }

    Ok(character)
}
