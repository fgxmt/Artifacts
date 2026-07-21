use std::collections::HashMap;

use reqwest::Client;

use crate::api::{equip_items, get_bank_items, move_character, unequip_items, wait_for_cooldown, withdraw_items};
use crate::flags::{Flag, FlagAction, GameState};
use crate::types::{Character, DepositItem, EquipBody, Result, UnequipBody};

use super::bank_ops::deposit_all;
use super::consumables::ensure_healing_consumables;
use super::crafting_exec::run_crafting_phase;
use super::crafting_plan::refresh_shared_crafting_state;
use super::merchant::run_merchant_stint;
use super::repositioning::{optimize_fight, optimize_gather};
use super::CharacterRole;

/// Current item code equipped in a named slot (weapon, ring1, artifact2, ...), or "" if empty.
fn slot_value<'a>(character: &'a Character, slot: &str) -> &'a str {
    match slot {
        "weapon"     => &character.weapon_slot,
        "shield"     => &character.shield_slot,
        "helmet"     => &character.helmet_slot,
        "body_armor" => &character.body_armor_slot,
        "leg_armor"  => &character.leg_armor_slot,
        "boots"      => &character.boots_slot,
        "ring1"      => &character.ring1_slot,
        "ring2"      => &character.ring2_slot,
        "amulet"     => &character.amulet_slot,
        "artifact1"  => &character.artifact1_slot,
        "artifact2"  => &character.artifact2_slot,
        "artifact3"  => &character.artifact3_slot,
        "utility1"   => &character.utility1_slot,
        "utility2"   => &character.utility2_slot,
        "rune"       => &character.rune_slot,
        "bag"        => &character.bag_slot,
        _            => "",
    }
}

/// Checks and processes any flags set for this character.
/// Every `EquipUpgrade` flag drained together is handled as a single bank trip — deposit
/// everything held, unequip every affected slot in one call, withdraw every needed item in one
/// call, then equip everything in one call — rather than a separate round trip per item. Moves
/// back to the character's pre-flag position afterward, unless a flag re-targeted the character
/// somewhere else (equipping an upgrade re-runs the character's optimizer, which moves them to
/// their new optimal spot).
pub(crate) async fn handle_flags(
    state: &GameState,
    name: &'static str,
    client: &Client,
    character: &mut Character,
    role: CharacterRole,
) -> Result<()> {
    let pending = state.drain_flags(name).await;
    if pending.is_empty() { return Ok(()); }

    let return_x = character.x;
    let return_y = character.y;
    let mut repositioned = false;

    let mut upgrades: Vec<(String, String)> = Vec::new(); // (slot, item_code)
    let mut other_flags: Vec<Flag> = Vec::new();

    for flag in pending {
        match flag.action {
            FlagAction::EquipUpgrade { slot, item_code } => upgrades.push((slot, item_code)),
            other => other_flags.push(Flag { from_character: flag.from_character, action: other }),
        }
    }

    if !upgrades.is_empty() {
        for (slot, item_code) in &upgrades {
            println!(
                "[{}] Flag: EquipUpgrade {{ slot: {:?}, item_code: {:?} }}",
                crate::ts_char(name), slot, item_code
            );
        }

        // Deposit everything currently held first, so there's maximum room for the old
        // (unequipped) and new (withdrawn) items this trip is about to juggle.
        *character = deposit_all(client, name, character.clone(), state).await?;

        let unequip_body: Vec<UnequipBody> = upgrades.iter()
            .filter(|(slot, _)| !slot_value(character, slot).is_empty())
            .map(|(slot, _)| UnequipBody { slot: slot.clone(), quantity: 1 })
            .collect();
        if !unequip_body.is_empty() {
            let result = unequip_items(client, name, unequip_body).await?;
            wait_for_cooldown(&result.cooldown).await;
            *character = result.character;
        }

        // These flags exist because the items were seen sitting in the bank — equip pulls from
        // inventory, not the bank directly, so everything needed has to be withdrawn first or the
        // equip call below fails with "Missing required item(s)". Combine quantities in case the
        // same item code is somehow needed for more than one slot.
        let mut withdraw_qty: HashMap<String, i32> = HashMap::new();
        for (_, item_code) in &upgrades {
            *withdraw_qty.entry(item_code.clone()).or_insert(0) += 1;
        }
        let withdraw_list: Vec<DepositItem> = withdraw_qty.into_iter()
            .map(|(code, quantity)| DepositItem { code, quantity })
            .collect();
        let result = withdraw_items(client, name, withdraw_list).await?;
        wait_for_cooldown(&result.cooldown).await;
        *character = result.character;
        if let Ok(bank) = get_bank_items(client).await { state.update_bank(bank).await; }

        let equip_body: Vec<EquipBody> = upgrades.iter()
            .map(|(slot, item_code)| EquipBody { code: item_code.clone(), slot: slot.clone(), quantity: 1 })
            .collect();
        let result = equip_items(client, name, equip_body).await?;
        wait_for_cooldown(&result.cooldown).await;
        *character = result.character;

        // Record these immediately (not just whenever set_item_ratings next runs) so a spare
        // copy of the same item still sitting in the bank doesn't get re-flagged as an "upgrade"
        // the moment this deposit/withdraw cycle refreshes the bank cache.
        for (slot, item_code) in &upgrades {
            state.set_equipped_slot(name, slot, item_code);
        }

        match role {
            CharacterRole::Combat => { optimize_fight(client, name, character, state, false).await?; }
            CharacterRole::Gathering(skill) => { optimize_gather(client, name, character, skill, state, false).await?; }
        }
        repositioned = true;
    }

    for flag in other_flags {
        println!(
            "[{}] Flag from '{}': {:?}",
            crate::ts_char(name), flag.from_character, flag.action
        );

        if character.x != 4 || character.y != 1 {
            let result = move_character(client, name, 4, 1).await?;
            wait_for_cooldown(&result.cooldown).await;
            *character = result.character;
        }

        match &flag.action {
            FlagAction::RetrieveFromBank(items) => {
                println!(
                    "[{}] TODO: withdraw {} item type(s) from bank: {:?}",
                    crate::ts_char(name),
                    items.len(),
                    items.iter().map(|i| format!("{}x {}", i.quantity, i.code)).collect::<Vec<_>>()
                );
                // TODO: call bank withdraw endpoint per item
            }
            FlagAction::HealingConsumablesAvailable => {
                *character = ensure_healing_consumables(client, name, character.clone(), state).await?;
            }
            FlagAction::CraftWishlistGearAvailable => {
                *character = run_crafting_phase(client, name, character.clone(), state).await?;
                refresh_shared_crafting_state(state, character);
            }
            FlagAction::MerchantSummon => {
                *character = run_merchant_stint(name, character.clone()).await?;
            }
            FlagAction::EquipUpgrade { .. } => unreachable!("EquipUpgrade flags are handled in the batch above"),
        }
    }

    if !repositioned && (character.x != return_x || character.y != return_y) {
        let result = move_character(client, name, return_x, return_y).await?;
        wait_for_cooldown(&result.cooldown).await;
        *character = result.character;
    }

    Ok(())
}
