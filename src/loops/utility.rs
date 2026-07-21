use reqwest::Client;

use crate::api::{equip_items, get_bank_items, move_character, wait_for_cooldown, withdraw_items, GameData};
use crate::flags::GameState;
use crate::optimize::meets_conditions;
use crate::types::{BankItem, Character, DepositItem, EquipBody, Item, Result};

use super::bank_ops::deposit_all;

const MAX_UTILITY_STACK: i32 = 100;

/// A (non-splash) health potion: something that restores the character's own HP, as opposed to
/// `splash_restore` (heals a party member) or any other utility effect (damage/resistance boosts,
/// antidotes, teleports, ...) that the battle simulator doesn't model.
fn is_health_potion(item: &Item) -> bool {
    item.effects.iter().any(|e| e.code == "restore")
}

fn slot_is_empty(code: &str, quantity: i32) -> bool {
    code.is_empty() || quantity <= 0
}

type UtilityPick = (String, i32);

/// Desired (code, quantity) for utility1/utility2, given which slots are actually up for grabs
/// (`slot1_free`/`slot2_free` — a slot still holding charges is left untouched and its item is
/// excluded from the other slot's candidate pool, since the two slots can never share a code).
/// Ranks every eligible-and-in-stock utility item by level descending, and — unless a locked slot
/// already holds one — forces the best available (non-splash) health potion into the first pick,
/// so the pair always includes one wherever that's possible at all.
fn pick_utility_loadout(
    character: &Character,
    data: &GameData,
    bank: &[BankItem],
    slot1_free: bool,
    slot2_free: bool,
) -> (Option<UtilityPick>, Option<UtilityPick>) {
    let bank_qty = |code: &str| bank.iter().find(|b| b.code == code).map(|b| b.quantity).unwrap_or(0);

    let locked_codes: Vec<&str> = [
        (!slot1_free).then_some(character.utility1_slot.as_str()),
        (!slot2_free).then_some(character.utility2_slot.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter(|c| !c.is_empty())
    .collect();

    let locked_has_health = locked_codes.iter().any(|code| {
        data.items.iter().find(|i| i.code == *code).is_some_and(is_health_potion)
    });

    let mut candidates: Vec<&Item> = data.items.iter()
        .filter(|i| i.item_type == "utility")
        .filter(|i| meets_conditions(character, i))
        .filter(|i| bank_qty(&i.code) > 0)
        .filter(|i| !locked_codes.contains(&i.code.as_str()))
        .collect();
    candidates.sort_by_key(|i| std::cmp::Reverse(i.level));

    let mut picks: Vec<UtilityPick> = Vec::new();

    if !locked_has_health {
        if let Some(h) = candidates.iter().find(|i| is_health_potion(i)) {
            picks.push((h.code.clone(), bank_qty(&h.code).min(MAX_UTILITY_STACK)));
        }
    }
    for item in &candidates {
        if picks.len() >= 2 { break; }
        if picks.iter().any(|(code, _)| code == &item.code) { continue; }
        picks.push((item.code.clone(), bank_qty(&item.code).min(MAX_UTILITY_STACK)));
    }

    let mut picks = picks.into_iter();
    (
        if slot1_free { picks.next() } else { None },
        if slot2_free { picks.next() } else { None },
    )
}

/// Withdraws and equips `requests` (slot, code, desired quantity), splitting into as many passes
/// as needed when the full amount can't fit in inventory at once — each pass withdraws and
/// equips only as much of each request as currently fits (bounded by the usual 20-slot /
/// `inventory_max_items` budget), which frees that same inventory room back up as the withdrawn
/// items move into the utility slots, then repeats with whatever's left until every request is
/// fully satisfied or a pass makes no progress at all (nothing fits, e.g. inventory is genuinely
/// full of something else). Assumes the character is already at the bank. Returns the (possibly
/// updated) character and whether anything was actually equipped.
async fn withdraw_and_equip_utility(
    client: &Client,
    name: &'static str,
    character: Character,
    state: &GameState,
    mut requests: Vec<(&'static str, String, i32)>,
) -> Result<(Character, bool)> {
    let mut character = character;
    let mut equipped_anything = false;

    loop {
        requests.retain(|(_, _, remaining)| *remaining > 0);
        if requests.is_empty() { break; }

        let used_qty: i32 = character.inventory.iter().map(|s| s.quantity).sum();
        let used_slots = character.inventory.iter().filter(|s| s.quantity > 0 && !s.code.is_empty()).count();
        let mut room_qty = (character.inventory_max_items - used_qty).max(0);
        let mut room_slots = 20usize.saturating_sub(used_slots);

        let mut withdraw_list: Vec<DepositItem> = Vec::new();
        let mut equip_body: Vec<EquipBody> = Vec::new();

        for (slot, code, remaining) in requests.iter_mut() {
            if room_qty <= 0 || room_slots == 0 { break; }
            let take = (*remaining).min(room_qty);
            if take <= 0 { continue; }

            withdraw_list.push(DepositItem { code: code.clone(), quantity: take });
            equip_body.push(EquipBody { code: code.clone(), slot: slot.to_string(), quantity: take });
            *remaining -= take;
            room_qty -= take;
            room_slots -= 1;
        }

        if equip_body.is_empty() { break; }

        let result = withdraw_items(client, name, withdraw_list).await?;
        wait_for_cooldown(&result.cooldown).await;
        if let Ok(bank) = get_bank_items(client).await { state.update_bank(bank).await; }

        let result = equip_items(client, name, equip_body).await?;
        wait_for_cooldown(&result.cooldown).await;
        character = result.character;
        equipped_anything = true;
    }

    Ok((character, equipped_anything))
}

/// Tops up utility1/utility2 from the bank wherever a slot is empty or has run out of charges — a
/// slot that still holds stock is left alone. Whenever there's anything to restock, deposits
/// everything (items and gold) first, same as any other bank trip, then withdraws and equips
/// whatever's available (see `withdraw_and_equip_utility` for how it handles more being available
/// than currently fits in inventory); if a slot needs restocking but the bank has nothing eligible
/// for it (e.g. no health potion crafted yet), it's simply left empty rather than blocking the
/// loop. Returns the (possibly updated) character and whether anything was actually equipped.
pub(crate) async fn ensure_utility_loadout(
    client: &Client,
    name: &'static str,
    character: Character,
    state: &GameState,
) -> Result<(Character, bool)> {
    let slot1_free = slot_is_empty(&character.utility1_slot, character.utility1_slot_quantity);
    let slot2_free = slot_is_empty(&character.utility2_slot, character.utility2_slot_quantity);
    if !slot1_free && !slot2_free {
        return Ok((character, false));
    }

    let mut character = deposit_all(client, name, character, state).await?;

    // deposit_all only moves to the bank if it actually had something to deposit — make sure
    // we're there regardless, since we're about to withdraw either way.
    if character.x != 4 || character.y != 1 {
        let result = move_character(client, name, 4, 1).await?;
        wait_for_cooldown(&result.cooldown).await;
        character = result.character;
    }

    let bank = state.bank_snapshot().await;
    let (pick1, pick2) = pick_utility_loadout(&character, &state.data, &bank, slot1_free, slot2_free);

    let mut requests: Vec<(&'static str, String, i32)> = Vec::new();
    if let Some((code, qty)) = pick1 { requests.push(("utility1", code, qty)); }
    if let Some((code, qty)) = pick2 { requests.push(("utility2", code, qty)); }

    if requests.is_empty() {
        println!("[{}] No eligible utility item available in the bank to restock.", crate::ts_char(name));
        return Ok((character, false));
    }

    withdraw_and_equip_utility(client, name, character, state, requests).await
}

/// Whenever the character is already at the bank (or about to be) for some other reason, tops
/// up any utility slot that's below a full 100-stack of whatever it already holds — never
/// swapping the item, just withdrawing and equipping more of the same code (see
/// `withdraw_and_equip_utility` for how it handles more being available than currently fits in
/// inventory). A slot that's genuinely empty is `ensure_utility_loadout`'s job, not this one —
/// this only ever tops up a slot that already holds something. Returns the (possibly updated)
/// character and whether anything was actually topped up.
pub(crate) async fn top_up_utility_stock(
    client: &Client,
    name: &'static str,
    character: Character,
    state: &GameState,
) -> Result<(Character, bool)> {
    let bank = state.bank_snapshot().await;
    let bank_qty = |code: &str| bank.iter().find(|b| b.code == code).map(|b| b.quantity).unwrap_or(0);

    let mut requests: Vec<(&'static str, String, i32)> = Vec::new();
    for (slot, code, quantity) in [
        ("utility1", character.utility1_slot.as_str(), character.utility1_slot_quantity),
        ("utility2", character.utility2_slot.as_str(), character.utility2_slot_quantity),
    ] {
        if code.is_empty() || quantity >= MAX_UTILITY_STACK { continue; }
        let need = (MAX_UTILITY_STACK - quantity).min(bank_qty(code));
        if need > 0 { requests.push((slot, code.to_string(), need)); }
    }

    if requests.is_empty() {
        return Ok((character, false));
    }

    let mut character = character;
    if character.x != 4 || character.y != 1 {
        let result = move_character(client, name, 4, 1).await?;
        wait_for_cooldown(&result.cooldown).await;
        character = result.character;
    }

    withdraw_and_equip_utility(client, name, character, state, requests).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::GameData;
    use crate::types::{ItemCondition, ItemEffect};

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

    fn utility_item(code: &str, level: i32, effect_code: &str) -> Item {
        Item {
            name: code.into(), code: code.into(), level, item_type: "utility".into(),
            subtype: "potion".into(), description: "".into(),
            conditions: vec![ItemCondition { code: "level".into(), operator: "gt".into(), value: level - 1 }],
            effects: vec![ItemEffect { code: effect_code.into(), value: 10, description: "".into() }],
            craft: None, tradeable: true, recyclable: false,
        }
    }

    fn blank_data(items: Vec<Item>) -> GameData {
        GameData { monsters: vec![], items, resources: vec![], maps: vec![], craftable_equip: vec![] }
    }

    fn bank(entries: &[(&str, i32)]) -> Vec<BankItem> {
        entries.iter().map(|(code, qty)| BankItem { code: code.to_string(), quantity: *qty }).collect()
    }

    /// The highest-level eligible item overall is a damage-boost potion, not a health potion —
    /// the hard "at least one health potion" requirement must still force one into the loadout
    /// rather than just taking the top 2 by level.
    #[test]
    fn forces_a_health_potion_even_when_outranked_by_level() {
        let character = blank_character();
        let data = blank_data(vec![
            utility_item("fire_boost_potion", 40, "boost_dmg_fire"),
            utility_item("small_health_potion", 5, "restore"),
        ]);
        let bank = bank(&[("fire_boost_potion", 50), ("small_health_potion", 20)]);

        let (p1, p2) = pick_utility_loadout(&character, &data, &bank, true, true);

        assert_eq!(p1, Some(("small_health_potion".to_string(), 20)));
        assert_eq!(p2, Some(("fire_boost_potion".to_string(), 50)));
    }

    /// With both slots free and multiple health potions eligible, the highest-level one is picked
    /// for the forced health slot, and the next-highest-level distinct item (here, another health
    /// potion) fills the other — never the same code twice.
    #[test]
    fn prioritizes_highest_level_and_never_duplicates_a_code() {
        let character = blank_character();
        let data = blank_data(vec![
            utility_item("small_health_potion", 5, "restore"),
            utility_item("minor_health_potion", 20, "restore"),
        ]);
        let bank = bank(&[("small_health_potion", 100), ("minor_health_potion", 40)]);

        let (p1, p2) = pick_utility_loadout(&character, &data, &bank, true, true);

        assert_eq!(p1, Some(("minor_health_potion".to_string(), 40)));
        assert_eq!(p2, Some(("small_health_potion".to_string(), 100)));
    }

    /// When one slot is locked holding a non-health item, the free slot must still be forced into
    /// a health potion — the constraint is about the pair as a whole, not the slot being restocked.
    #[test]
    fn forces_health_into_the_free_slot_when_locked_slot_is_not_one() {
        let mut character = blank_character();
        character.utility1_slot = "fire_boost_potion".into();
        character.utility1_slot_quantity = 12;

        let data = blank_data(vec![
            utility_item("fire_boost_potion", 40, "boost_dmg_fire"),
            utility_item("small_health_potion", 5, "restore"),
        ]);
        let bank_snapshot = bank(&[("fire_boost_potion", 50), ("small_health_potion", 20)]);

        let (p1, p2) = pick_utility_loadout(&character, &data, &bank_snapshot, false, true);

        assert_eq!(p1, None, "locked slot must not be reassigned");
        assert_eq!(p2, Some(("small_health_potion".to_string(), 20)));
    }

    /// When the locked slot is already a health potion, the free slot doesn't need to be one too
    /// — it should just take the best remaining distinct item.
    #[test]
    fn does_not_force_a_second_health_potion_when_one_is_already_locked_in() {
        let mut character = blank_character();
        character.utility1_slot = "small_health_potion".into();
        character.utility1_slot_quantity = 5;

        let data = blank_data(vec![
            utility_item("fire_boost_potion", 40, "boost_dmg_fire"),
            utility_item("small_health_potion", 5, "restore"),
        ]);
        let bank_snapshot = bank(&[("fire_boost_potion", 50), ("small_health_potion", 20)]);

        let (p1, p2) = pick_utility_loadout(&character, &data, &bank_snapshot, false, true);

        assert_eq!(p1, None);
        assert_eq!(p2, Some(("fire_boost_potion".to_string(), 50)));
    }

    /// Best-effort: if no health potion is available in the bank at all, the constraint can't be
    /// honored — the loop should still equip whatever eligible items it can rather than nothing.
    #[test]
    fn best_effort_when_no_health_potion_is_available() {
        let character = blank_character();
        let data = blank_data(vec![
            utility_item("fire_boost_potion", 40, "boost_dmg_fire"),
            utility_item("small_health_potion", 5, "restore"),
        ]);
        // Bank has the boost potion but zero health potions in stock.
        let bank_snapshot = bank(&[("fire_boost_potion", 50), ("small_health_potion", 0)]);

        let (p1, p2) = pick_utility_loadout(&character, &data, &bank_snapshot, true, true);

        assert_eq!(p1, Some(("fire_boost_potion".to_string(), 50)));
        assert_eq!(p2, None);
    }

    /// Quantity is capped at 100 even if the bank holds more.
    #[test]
    fn quantity_is_capped_at_max_stack() {
        let character = blank_character();
        let data = blank_data(vec![utility_item("small_health_potion", 5, "restore")]);
        let bank_snapshot = bank(&[("small_health_potion", 500)]);

        let (p1, _) = pick_utility_loadout(&character, &data, &bank_snapshot, true, true);

        assert_eq!(p1, Some(("small_health_potion".to_string(), 100)));
    }
}
