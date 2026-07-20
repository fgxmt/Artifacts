use std::collections::{HashMap, HashSet};

use reqwest::Client;

use crate::api::{craft_item, get_bank_items, is_inventory_full, move_character, wait_for_cooldown, withdraw_items};
use crate::flags::GameState;
use crate::optimize::{locations_raw_for, refining_skill_for, skill_level};
use crate::types::{BankItem, Character, DepositItem, Item, Result};

use super::bank_ops::{deposit_all, inventory_qty, print_plan};
use super::movement::move_to_nearest;

/// A batch of recipes to craft this trip, and exactly what still needs withdrawing from the
/// bank (i.e. isn't already held) to cover it.
pub(crate) struct RefiningPlan<'a> {
    pub(crate) crafts: Vec<(&'a Item, i32)>,
    pub(crate) to_withdraw: Vec<DepositItem>,
    pub(crate) reserved: HashMap<String, i32>,
}

/// Plans the biggest batch of `refine_skill` recipes (highest level first) craftable from
/// `character`'s held inventory plus the bank — bounded by whatever's reserved for equip-upgrade
/// or next-tier-wishlist recipes that aren't craftable yet (per `reserved`, `state`'s global
/// reservation cache — see `plan_crafting_priority`) and by the 20-slot / `inventory_max_items`
/// budget. Materials already held are used before anything is drawn from the bank.
pub(crate) fn plan_refining_batch<'a>(state: &'a GameState, bank: &[BankItem], character: &Character, reserved: &HashMap<String, i32>, refine_skill: &str) -> RefiningPlan<'a> {
    let mut recipes: Vec<&Item> = state.data.items.iter()
        .filter(|i| i.craft.as_ref().is_some_and(|c| c.skill.as_deref() == Some(refine_skill)))
        .filter(|i| i.craft.as_ref().is_some_and(|c| skill_level(character, refine_skill) >= c.level.unwrap_or(0)))
        .collect();
    recipes.sort_by_key(|item| std::cmp::Reverse(item.level));

    let mut projected_qty: HashMap<String, i32> = HashMap::new();
    for slot in &character.inventory {
        if slot.quantity > 0 && !slot.code.is_empty() {
            *projected_qty.entry(slot.code.clone()).or_insert(0) += slot.quantity;
        }
    }
    let mut projected_slots: HashSet<String> = projected_qty.keys().cloned().collect();
    let mut total_qty: i32 = projected_qty.values().sum();

    // Remaining combined (held + bank - reserved) supply per ingredient code, decremented as
    // each recipe is committed — without this, two recipes sharing an ingredient (e.g. sap and
    // ash_plank both needing ash_wood) would each be sized against the *full* supply, together
    // planning more than what's actually available.
    let mut remaining_supply: HashMap<String, i32> = HashMap::new();
    // Subset of `reserved` actually relevant to recipes considered here, for a focused print.
    let mut relevant_reserved: HashMap<String, i32> = HashMap::new();

    let mut crafts: Vec<(&Item, i32)> = Vec::new();
    let mut to_withdraw: HashMap<String, i32> = HashMap::new();

    for item in recipes {
        let craft = item.craft.as_ref().unwrap();
        let mut qty = craft.items.iter()
            .map(|ing| {
                let avail = *remaining_supply.entry(ing.code.clone()).or_insert_with(|| {
                    let bank_amt = bank.iter().find(|b| b.code == ing.code).map(|b| b.quantity).unwrap_or(0);
                    let held_amt = inventory_qty(&character.inventory, &ing.code);
                    let reserved_amt = reserved.get(&ing.code).copied().unwrap_or(0);
                    if reserved_amt > 0 {
                        relevant_reserved.insert(ing.code.clone(), reserved_amt);
                    }
                    (held_amt + bank_amt - reserved_amt).max(0)
                });
                avail / ing.quantity.max(1)
            })
            .min().unwrap_or(0);

        while qty > 0 {
            let mut extra_slots = if projected_slots.contains(&item.code) { 0 } else { 1 };
            let mut extra_qty = 0;
            for ing in &craft.items {
                let need = ing.quantity * qty;
                let have = projected_qty.get(&ing.code).copied().unwrap_or(0);
                if need > have {
                    if !projected_slots.contains(&ing.code) { extra_slots += 1; }
                    extra_qty += need - have;
                }
            }
            if projected_slots.len() as i32 + extra_slots <= 20 && total_qty + extra_qty <= character.inventory_max_items {
                break;
            }
            qty -= 1;
        }
        if qty <= 0 { continue; }

        for ing in &craft.items {
            let need = ing.quantity * qty;

            *remaining_supply.entry(ing.code.clone()).or_insert(0) -= need;

            let have = projected_qty.entry(ing.code.clone()).or_insert(0);
            if need > *have {
                let deficit = need - *have;
                *to_withdraw.entry(ing.code.clone()).or_insert(0) += deficit;
                total_qty += deficit;
                projected_slots.insert(ing.code.clone());
                *have = need;
            }
        }
        projected_slots.insert(item.code.clone());
        crafts.push((item, qty));
    }

    let to_withdraw = to_withdraw.into_iter().map(|(code, quantity)| DepositItem { code, quantity }).collect();
    RefiningPlan { crafts, to_withdraw, reserved: relevant_reserved }
}

/// Refines raw materials into the highest-level recipe(s) available for `gather_skill`'s
/// refined material (woodcutting/mining refine into their own skill; fishing refines via
/// cooking), batching as many recipes into each trip as inventory allows. Starts from whatever
/// `character` already holds (from gathering) and only visits the bank if more is needed or the
/// inventory is genuinely full — so a not-full inventory goes straight to the workstation.
/// Reserves whatever other characters' crafting-upgrade recipes need, making as many
/// bank/workstation trips as necessary until nothing more can be refined, then deposits
/// whatever's left.
pub(crate) async fn run_refining_phase(
    client: &Client,
    name: &'static str,
    mut character: Character,
    state: &GameState,
    gather_skill: &str,
) -> Result<Character> {
    let refine_skill = refining_skill_for(gather_skill);
    if refine_skill.is_empty() { return Ok(character); }

    let refine_locs = locations_raw_for(&state.data.maps, "workshop", refine_skill);
    if refine_locs.is_empty() { return Ok(character); }

    loop {
        if is_inventory_full(&character) {
            character = deposit_all(client, name, character, state).await?;
        }

        let bank = state.bank_snapshot().await;
        let reserved = state.reserved_materials_snapshot();
        let plan = plan_refining_batch(state, &bank, &character, &reserved, refine_skill);
        if plan.crafts.is_empty() { break; }

        let planned: Vec<(&str, i32)> = plan.crafts.iter().map(|(item, qty)| (item.code.as_str(), *qty)).collect();
        print_plan(name, &planned, &plan.reserved);

        if !plan.to_withdraw.is_empty() {
            if character.x != 4 || character.y != 1 {
                let result = move_character(client, name, 4, 1).await?;
                wait_for_cooldown(&result.cooldown).await;
            }
            println!("[{}] Withdrawing {} material stack(s) to refine...", crate::ts_char(name), plan.to_withdraw.len());
            let result = withdraw_items(client, name, plan.to_withdraw).await?;
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;
            if let Ok(bank) = get_bank_items(client).await { state.update_bank(bank).await; }
        }

        move_to_nearest(client, name, &mut character, &refine_locs, refine_skill).await?;
        for (item, qty) in &plan.crafts {
            let result = craft_item(client, name, &item.code, *qty).await?;
            wait_for_cooldown(&result.cooldown).await;
            character = result.character;
        }
    }

    deposit_all(client, name, character, state).await
}
