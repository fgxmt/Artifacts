use std::collections::HashMap;

use reqwest::Client;

use crate::api::{deposit_gold, deposit_items, get_bank_items, move_character, wait_for_cooldown};
use crate::flags::GameState;
use crate::types::{Character, DepositItem, InventorySlot, Result};

/// Total quantity of `code` currently held across `inventory`.
pub(crate) fn inventory_qty(inventory: &[InventorySlot], code: &str) -> i32 {
    inventory.iter().filter(|s| s.code == code).map(|s| s.quantity).sum()
}

/// Deposits everything in `character`'s inventory and all held gold at the bank (moving there
/// first if needed), and refreshes the cached bank snapshot.
pub(crate) async fn deposit_all(client: &Client, name: &'static str, character: Character, state: &GameState) -> Result<Character> {
    let items_to_deposit: Vec<DepositItem> = character.inventory.iter()
        .filter(|s| s.quantity > 0 && !s.code.is_empty())
        .map(|s| DepositItem { code: s.code.clone(), quantity: s.quantity })
        .collect();
    if items_to_deposit.is_empty() && character.gold <= 0 { return Ok(character); }

    if character.x != 4 || character.y != 1 {
        let result = move_character(client, name, 4, 1).await?;
        wait_for_cooldown(&result.cooldown).await;
    }

    let mut character = character;
    if !items_to_deposit.is_empty() {
        println!("[{}] Depositing {} item stack(s) to bank...", crate::ts_char(name), items_to_deposit.len());
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

    Ok(character)
}

/// Prints a crafting/refining plan: what's about to be made and how much, plus what's being
/// held back in the bank for other characters' upgrade recipes.
pub(crate) fn print_plan(name: &str, planned: &[(&str, i32)], reserved: &HashMap<String, i32>) {
    if planned.is_empty() {
        println!("[{}] Plan: nothing craftable right now.", crate::ts_char(name));
    } else {
        let planned_str: Vec<String> = planned.iter().map(|(code, qty)| format!("{}x {}", qty, code)).collect();
        println!("[{}] Planned crafts: {}", crate::ts_char(name), planned_str.join(", "));
    }

    if reserved.is_empty() {
        println!("[{}] Reserved for upgrades: none", crate::ts_char(name));
    } else {
        let mut reserved_str: Vec<String> = reserved.iter()
            .filter(|(_, qty)| **qty > 0)
            .map(|(code, qty)| format!("{}x {}", qty, code))
            .collect();
        reserved_str.sort();
        println!("[{}] Reserved for upgrades: {}", crate::ts_char(name), reserved_str.join(", "));
    }
}
