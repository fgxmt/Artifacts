use reqwest::Client;

use crate::api::{get_bank_items, move_character, rest_character, use_item, wait_for_cooldown, withdraw_items, GameData};
use crate::flags::GameState;
use crate::optimize::{is_healing_consumable, meets_conditions};
use crate::types::{BankItem, Character, DepositItem, Item, Result};

use super::bank_ops::deposit_all;
use super::utility::{ensure_utility_loadout, top_up_utility_stock};

/// Leaves some inventory headroom for whatever fights drop along the way, rather than packing
/// every last slot with food.
const INVENTORY_FILL_TARGET_PCT: f64 = 0.95;

/// At most this many distinct inventory slots may hold healing consumables at once — keeps a
/// character from hoarding half a dozen different food tiers instead of leaving room for loot.
const MAX_HEALING_CONSUMABLE_SLOTS: usize = 3;

fn heal_value(item: &Item) -> i32 {
    item.effects.iter().find(|e| e.code == "heal").map(|e| e.value).unwrap_or(0)
}

fn inventory_healing_consumable_qty(character: &Character, data: &GameData) -> i32 {
    character.inventory.iter()
        .filter(|s| s.quantity > 0)
        .filter(|s| data.items.iter().find(|i| i.code == s.code).is_some_and(is_healing_consumable))
        .map(|s| s.quantity)
        .sum()
}

fn bank_has_healing_consumable(bank: &[BankItem], data: &GameData) -> bool {
    bank.iter().any(|b| {
        b.quantity > 0 && data.items.iter().find(|i| i.code == b.code).is_some_and(is_healing_consumable)
    })
}

/// How many of a `value`-HP-per-use consumable to eat in one `/use` call to cross
/// `min_hp_threshold` from `hp` — the fewest that still guarantee crossing it, capped at
/// `held_qty` (never eat what isn't held, and always eat at least 1 while below threshold).
fn heal_quantity_needed(hp: i32, min_hp_threshold: i32, value: i32, held_qty: i32) -> i32 {
    let needed = ((min_hp_threshold - hp) as f64 / value.max(1) as f64).ceil() as i32;
    needed.clamp(1, held_qty.max(1))
}

/// Plans what to withdraw to fill up to `target_qty` total items with healing consumables the
/// character actually meets the level condition for — highest heal-value first, so later healing
/// costs the fewest possible `/use` calls — bounded by `held_qty` (already-held items counting
/// toward `target_qty`), `used_slots` (already-used inventory slots counting toward the 20-slot
/// budget), and capped at `MAX_HEALING_CONSUMABLE_SLOTS` distinct item types regardless of how
/// much general inventory room is left, so a character never hoards half a dozen different food
/// tiers instead of leaving room for loot.
fn plan_healing_consumable_withdrawal(
    character: &Character,
    data: &GameData,
    bank: &[BankItem],
    target_qty: i32,
    held_qty: i32,
    used_slots: usize,
) -> Vec<(String, i32)> {
    let mut candidates: Vec<&Item> = data.items.iter()
        .filter(|i| is_healing_consumable(i))
        .filter(|i| meets_conditions(character, i))
        .filter(|i| bank.iter().any(|b| b.code == i.code && b.quantity > 0))
        .collect();
    candidates.sort_by_key(|i| std::cmp::Reverse(heal_value(i)));

    let mut room_qty = (target_qty - held_qty).max(0);
    let mut room_slots = 20usize.saturating_sub(used_slots).min(MAX_HEALING_CONSUMABLE_SLOTS);

    let mut withdraw_list = Vec::new();
    for item in candidates {
        if room_qty <= 0 || room_slots == 0 { break; }
        let available = bank.iter().find(|b| b.code == item.code).map(|b| b.quantity).unwrap_or(0);
        let take = available.min(room_qty);
        if take <= 0 { continue; }

        room_qty -= take;
        room_slots -= 1;
        withdraw_list.push((item.code.clone(), take));
    }

    withdraw_list
}

/// If the character's inventory is out of healing consumables (food, not utility potions) and
/// the bank has at least one, deposits everything (items + gold, restocking/topping-up utilities
/// while there too), then fills up to 95% of inventory capacity with healing consumables —
/// highest heal-value first, so later healing costs the fewest possible `/use` calls — bounded by
/// the usual 20-slot / `inventory_max_items` budget. If the bank also has none, registers the
/// character as waiting (see `GameState::await_healing_consumables`) instead of visiting for
/// nothing, and simply leaves inventory as-is; the fight loop keeps going with no consumables on
/// hand until another character's bank trip flags one available. A no-op if the character already
/// holds some.
pub(crate) async fn ensure_healing_consumables(
    client: &Client,
    name: &'static str,
    character: Character,
    state: &GameState,
) -> Result<Character> {
    if inventory_healing_consumable_qty(&character, &state.data) > 0 {
        return Ok(character);
    }

    let bank = state.bank_snapshot().await;
    if !bank_has_healing_consumable(&bank, &state.data) {
        state.await_healing_consumables(name);
        return Ok(character);
    }

    let character = deposit_all(client, name, character, state).await?;
    let (character, _) = ensure_utility_loadout(client, name, character, state).await?;
    let (mut character, _) = top_up_utility_stock(client, name, character, state).await?;

    if character.x != 4 || character.y != 1 {
        let result = move_character(client, name, 4, 1).await?;
        wait_for_cooldown(&result.cooldown).await;
        character = result.character;
    }

    let bank = state.bank_snapshot().await;
    let target_qty = (character.inventory_max_items as f64 * INVENTORY_FILL_TARGET_PCT).floor() as i32;
    let held_qty: i32 = character.inventory.iter().map(|s| s.quantity).sum();
    let used_slots = character.inventory.iter().filter(|s| s.quantity > 0 && !s.code.is_empty()).count();

    let withdraw_list: Vec<DepositItem> = plan_healing_consumable_withdrawal(&character, &state.data, &bank, target_qty, held_qty, used_slots)
        .into_iter()
        .map(|(code, quantity)| DepositItem { code, quantity })
        .collect();

    if withdraw_list.is_empty() {
        return Ok(character);
    }

    let result = withdraw_items(client, name, withdraw_list).await?;
    wait_for_cooldown(&result.cooldown).await;
    if let Ok(bank) = get_bank_items(client).await { state.update_bank(bank).await; }

    Ok(result.character)
}

/// Uses held healing consumables — highest heal-value first — until HP crosses
/// `min_hp_threshold` or none remain, replacing a full rest with eating just enough to safely
/// fight on. Falls back to resting only if no healing consumables are on hand at all (e.g. the
/// bank had none either, per `ensure_healing_consumables`). A no-op if HP is already at or above
/// the threshold.
pub(crate) async fn heal_to_threshold(
    client: &Client,
    name: &'static str,
    character: Character,
    min_hp_threshold: i32,
    state: &GameState,
) -> Result<Character> {
    let mut character = character;

    while character.hp < min_hp_threshold {
        let mut held: Vec<(&Item, i32)> = character.inventory.iter()
            .filter(|s| s.quantity > 0)
            .filter_map(|s| {
                let item = state.data.items.iter().find(|i| i.code == s.code)?;
                is_healing_consumable(item).then_some((item, s.quantity))
            })
            .collect();
        held.sort_by_key(|(item, _)| std::cmp::Reverse(heal_value(item)));

        let Some((item, held_qty)) = held.first().copied() else { break };
        let qty = heal_quantity_needed(character.hp, min_hp_threshold, heal_value(item), held_qty);

        println!(
            "[{}] HP ({}) below threshold ({}) — eating {}x {}.",
            crate::ts_char(name), character.hp, min_hp_threshold, qty, item.name
        );
        let result = use_item(client, name, &item.code, qty).await?;
        wait_for_cooldown(&result.cooldown).await;
        character = result.character;
    }

    if character.hp < min_hp_threshold {
        println!(
            "[{}] HP ({}) still below threshold ({}) with no healing consumables on hand — resting.",
            crate::ts_char(name), character.hp, min_hp_threshold
        );
        let rest = rest_character(client, name).await?;
        wait_for_cooldown(&rest.cooldown).await;
        character = rest.character;
    }

    Ok(character)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{InventorySlot, ItemCondition, ItemEffect};

    fn blank_character() -> Character {
        Character {
            name: "test".into(), account: "test".into(), skin: "".into(),
            level: 50, xp: 0, max_xp: 0, gold: 0, speed: 0,
            mining_level: 1, mining_xp: 0, mining_max_xp: 0,
            woodcutting_level: 1, woodcutting_xp: 0, woodcutting_max_xp: 0,
            fishing_level: 1, fishing_xp: 0, fishing_max_xp: 0,
            weaponcrafting_level: 1, weaponcrafting_xp: 0, weaponcrafting_max_xp: 0,
            gearcrafting_level: 1, gearcrafting_xp: 0, gearcrafting_max_xp: 0,
            jewelrycrafting_level: 1, jewelrycrafting_xp: 0, jewelrycrafting_max_xp: 0,
            cooking_level: 1, cooking_xp: 0, cooking_max_xp: 0,
            alchemy_level: 1, alchemy_xp: 0, alchemy_max_xp: 0,
            hp: 100, max_hp: 100, haste: 0, critical_strike: 0, wisdom: 0,
            prospecting: 0, initiative: 0, threat: 0,
            attack_fire: 0, attack_earth: 0, attack_water: 0, attack_air: 0,
            dmg: 0, dmg_fire: 0, dmg_earth: 0, dmg_water: 0, dmg_air: 0,
            res_fire: 0, res_earth: 0, res_water: 0, res_air: 0,
            effects: vec![], x: 0, y: 0, layer: "interior".into(), map_id: 0,
            cooldown: 0, cooldown_expiration: None,
            weapon_slot: "".into(), rune_slot: "".into(), shield_slot: "".into(),
            helmet_slot: "".into(), body_armor_slot: "".into(), leg_armor_slot: "".into(),
            boots_slot: "".into(), ring1_slot: "".into(), ring2_slot: "".into(),
            amulet_slot: "".into(), artifact1_slot: "".into(), artifact2_slot: "".into(),
            artifact3_slot: "".into(), utility1_slot: "".into(), utility1_slot_quantity: 0,
            utility2_slot: "".into(), utility2_slot_quantity: 0, bag_slot: "".into(),
            task: "".into(), task_type: "".into(), task_progress: 0, task_total: 0,
            inventory_max_items: 100, inventory: vec![],
        }
    }

    fn food_item(code: &str, level: i32, heal: i32) -> Item {
        Item {
            name: code.into(), code: code.into(), level, item_type: "consumable".into(),
            subtype: "food".into(), description: "".into(),
            conditions: vec![ItemCondition { code: "level".into(), operator: "gt".into(), value: level - 1 }],
            effects: vec![ItemEffect { code: "heal".into(), value: heal, description: "".into() }],
            craft: None, tradeable: true, recyclable: false,
        }
    }

    fn utility_potion(code: &str) -> Item {
        Item {
            name: code.into(), code: code.into(), level: 5, item_type: "utility".into(),
            subtype: "potion".into(), description: "".into(), conditions: vec![],
            effects: vec![ItemEffect { code: "restore".into(), value: 30, description: "".into() }],
            craft: None, tradeable: true, recyclable: false,
        }
    }

    fn blank_data(items: Vec<Item>) -> GameData {
        GameData { monsters: vec![], items, resources: vec![], maps: vec![], craftable_equip: vec![] }
    }

    #[test]
    fn heal_value_reads_the_heal_effect() {
        assert_eq!(heal_value(&food_item("cooked_chicken", 1, 80)), 80);
        // A utility potion's `restore` effect isn't `heal` — should read as 0, not be confused
        // with it.
        assert_eq!(heal_value(&utility_potion("small_health_potion")), 0);
    }

    /// Only `consumable`/`heal` items count — a utility health potion sitting in inventory
    /// shouldn't be mistaken for a healing consumable.
    #[test]
    fn inventory_qty_ignores_utility_potions() {
        let mut character = blank_character();
        character.inventory = vec![
            InventorySlot { slot: 1, code: "cooked_chicken".into(), quantity: 3 },
            InventorySlot { slot: 2, code: "small_health_potion".into(), quantity: 5 },
        ];
        let data = blank_data(vec![food_item("cooked_chicken", 1, 80), utility_potion("small_health_potion")]);

        assert_eq!(inventory_healing_consumable_qty(&character, &data), 3);
    }

    #[test]
    fn bank_has_healing_consumable_requires_positive_quantity() {
        let data = blank_data(vec![food_item("cooked_chicken", 1, 80)]);
        assert!(!bank_has_healing_consumable(&[BankItem { code: "cooked_chicken".into(), quantity: 0 }], &data));
        assert!(bank_has_healing_consumable(&[BankItem { code: "cooked_chicken".into(), quantity: 1 }], &data));
    }

    #[test]
    fn heal_quantity_needed_covers_the_gap_in_one_call() {
        // 50 hp missing, 80 hp/use -> 1 is enough.
        assert_eq!(heal_quantity_needed(50, 100, 80, 10), 1);
        // 200 hp missing, 80 hp/use -> ceil(200/80) = 3.
        assert_eq!(heal_quantity_needed(0, 200, 80, 10), 3);
        // Never eat more than what's held, even if it doesn't fully cover the gap.
        assert_eq!(heal_quantity_needed(0, 200, 80, 2), 2);
        // Always at least 1 while still below threshold.
        assert_eq!(heal_quantity_needed(99, 100, 80, 10), 1);
    }

    /// Even with abundant bank stock across many different food tiers and plenty of general
    /// inventory room, at most `MAX_HEALING_CONSUMABLE_SLOTS` distinct item types are ever taken.
    #[test]
    fn withdrawal_plan_never_exceeds_the_slot_cap() {
        let data = blank_data(vec![
            food_item("cooked_desert_scorpion_meat", 50, 800),
            food_item("cooked_hellhound_meat", 40, 600),
            food_item("fish_soup", 40, 500),
            food_item("cooked_salmon", 40, 400),
            food_item("cooked_rat_meat", 30, 400),
        ]);
        let bank = vec![
            BankItem { code: "cooked_desert_scorpion_meat".into(), quantity: 50 },
            BankItem { code: "cooked_hellhound_meat".into(), quantity: 50 },
            BankItem { code: "fish_soup".into(), quantity: 50 },
            BankItem { code: "cooked_salmon".into(), quantity: 50 },
            BankItem { code: "cooked_rat_meat".into(), quantity: 50 },
        ];

        // A large target so quantity is never the binding constraint — only the slot cap is.
        let plan = plan_healing_consumable_withdrawal(&blank_character(), &data, &bank, 1000, 0, 0);

        assert_eq!(plan.len(), MAX_HEALING_CONSUMABLE_SLOTS);
        // Highest heal-value first.
        let codes: Vec<&str> = plan.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(codes, vec!["cooked_desert_scorpion_meat", "cooked_hellhound_meat", "fish_soup"]);
    }

    /// The plan never asks for more than fits under `target_qty`, already-held items included.
    #[test]
    fn withdrawal_plan_respects_target_quantity() {
        let data = blank_data(vec![food_item("cooked_chicken", 1, 80)]);
        let bank = vec![BankItem { code: "cooked_chicken".into(), quantity: 500 }];

        let plan = plan_healing_consumable_withdrawal(&blank_character(), &data, &bank, 95, 90, 0);

        assert_eq!(plan, vec![("cooked_chicken".to_string(), 5)]);
    }
}
