use std::collections::{HashMap, HashSet};

use crate::flags::GameState;
use crate::formulas::{calculate_crafting_xp, craft_xp_per_ingredient, is_recyclable_for_wishlist, recycle_returns_per_ingredient, xp_to_next_level};
use crate::optimize::{current_slot_rating, skill_level, skill_xp_progress};
use crate::types::{Character, Item};

use super::bank_ops::print_plan;

pub(crate) const CRAFTING_SKILLS: [&str; 3] = ["weaponcrafting", "gearcrafting", "jewelrycrafting"];
pub(crate) const WISHLIST_SKILLS: [&str; 4] = ["weaponcrafting", "gearcrafting", "jewelrycrafting", "alchemy"];

/// Which of the three priority tiers a planned craft belongs to — equipment upgrades outrank
/// next-tier wishlist progress, which outranks generic XP-maximizing filler.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanTier {
    Upgrade,
    Wishlist,
    Filler,
}

/// One planned craft: an item code/skill and how many to make.
pub(crate) struct CraftPlanEntry {
    pub(crate) code: String,
    pub(crate) skill: String,
    /// Target quantity. `i32::MAX` for filler entries means "as much as bank/inventory allow".
    pub(crate) quantity: i32,
    pub(crate) tier: PlanTier,
}

/// Global-priority equipment-upgrade allocator. For every character's every slot, considers
/// *every* ranked alternative that's genuinely better than what's currently equipped there (not
/// just the top pick — see `current_slot_rating`) and is a weaponcrafting/gearcrafting/
/// jewelrycrafting item. All such (character, slot, alternative) steps across every character are
/// laid out in one list and sorted by rating descending — a global priority order, not scoped per
/// character or slot — then walked greedily: the first alternative for a given (character, slot)
/// that's actually craftable right now (skill level + `remaining_supply`, decremented as each is
/// committed) is planned and that slot is resolved, so lower-ranked alternatives for the same slot
/// are skipped; every alternative that fails craftability contributes its *direct* ingredients
/// (no recursion into sub-ingredients — refining toward a needed bar/plank can still proceed
/// freely) to the reservation, without resolving the slot, so a lower-ranked alternative still
/// gets its turn later in the walk if a higher one never pans out.
///
/// This means the highest-rated upgrade across *all* characters gets first claim on scarce shared
/// materials — if two different characters' upgrades both need the same scarce bar and only one
/// can be made, the higher-rated one wins, regardless of which character or slot it belongs to.
///
/// Returns (craftable entries, reserved direct ingredients, every item code considered at all —
/// for the caller's filler-exclusion set, since a still-blocked-but-demanded item must stay
/// excluded from filler even though it produced no craftable entry).
pub(crate) fn allocate_upgrade_crafts(
    state: &GameState,
    character: &Character,
    remaining_supply: &mut HashMap<String, i32>,
) -> (Vec<CraftPlanEntry>, HashMap<String, i32>, HashSet<String>) {
    struct Step { char_name: String, slot: String, code: String, skill: String, rating: f64 }

    let mut steps: Vec<Step> = Vec::new();
    for (char_name, ratings) in state.all_item_ratings() {
        let equipped = state.equipped_snapshot(&char_name);
        for r in &ratings {
            let current_code   = equipped.get(r.slot).map(|s| s.as_str());
            let current_rating = current_slot_rating(&r.ranked, current_code);

            for item in &r.ranked {
                if item.rating <= current_rating { continue; }
                if current_code == Some(item.code.as_str()) { continue; }
                let Some(craft_skill) = state.data.items.iter().find(|i| i.code == item.code)
                    .and_then(|i| i.craft.as_ref())
                    .and_then(|c| c.skill.as_deref())
                    .filter(|skill| CRAFTING_SKILLS.contains(skill))
                else { continue };

                steps.push(Step {
                    char_name: char_name.clone(),
                    slot: r.slot.to_string(),
                    code: item.code.clone(),
                    skill: craft_skill.to_string(),
                    rating: item.rating,
                });
            }
        }
    }

    steps.sort_by(|a, b| b.rating.partial_cmp(&a.rating).unwrap_or(std::cmp::Ordering::Equal));

    let mut resolved: HashSet<(String, String)> = HashSet::new();
    let mut craftable: HashMap<String, (String, i32)> = HashMap::new(); // code -> (skill, qty)
    let mut reserved: HashMap<String, i32> = HashMap::new();
    let mut all_codes: HashSet<String> = HashSet::new();

    for step in steps {
        all_codes.insert(step.code.clone());
        let slot_key = (step.char_name, step.slot);
        if resolved.contains(&slot_key) { continue; }

        let item  = match state.data.items.iter().find(|i| i.code == step.code) { Some(i) => i, None => continue };
        let craft = match &item.craft { Some(c) => c, None => continue };

        let level_ok = craft.level.is_none_or(|req| skill_level(character, &step.skill) >= req);
        let fully_suppliable = craft.items.iter().all(|ing| {
            remaining_supply.get(&ing.code).copied().unwrap_or(0) >= ing.quantity
        });

        if level_ok && fully_suppliable {
            for ing in &craft.items {
                *remaining_supply.entry(ing.code.clone()).or_insert(0) -= ing.quantity;
            }
            craftable.entry(step.code).or_insert_with(|| (step.skill, 0)).1 += 1;
            resolved.insert(slot_key);
        } else {
            for ing in &craft.items {
                *reserved.entry(ing.code.clone()).or_insert(0) += ing.quantity;
            }
        }
    }

    let entries = craftable.into_iter()
        .map(|(code, (skill, quantity))| CraftPlanEntry { code, skill, quantity, tier: PlanTier::Upgrade })
        .collect();

    (entries, reserved, all_codes)
}

/// XP still needed for a crafting skill at (`level`, `xp`, `max_xp`) to reach the next tier — the
/// next multiple of 5 strictly above `level` (if `level` is itself a multiple of 5, targets the
/// next one, a fresh 5-level climb).
pub(crate) fn xp_to_next_tier(level: i32, xp: i32, max_xp: i32) -> i32 {
    let target = if level % 5 == 0 { level + 5 } else { (level / 5 + 1) * 5 };
    let mut remaining = (max_xp - xp).max(0);
    for lvl in (level + 1)..target {
        remaining += xp_to_next_level(lvl);
    }
    remaining
}

/// Next-tier material wishlist: for each of the 4 crafting skills, projects how many crafts of
/// *every* currently-craftable recipe in that skill would be needed to close the remaining XP gap
/// to the next tier, at a static snapshot of the current skill level (an accepted approximation —
/// this recomputes fresh on every planning pass, so it self-corrects as real XP/levels change; it
/// doesn't model level-penalty shifts mid-batch). Every qualifying recipe across all 4 skills
/// contributes its own entry — this is a union of every viable path, not one committed choice, so
/// e.g. two alternative recipes that could each independently close the same skill's tier gap both
/// appear in full.
pub(crate) fn build_wishlist(state: &GameState, character: &Character) -> Vec<CraftPlanEntry> {
    let mut entries = Vec::new();

    for &skill in &WISHLIST_SKILLS {
        let (level, xp, max_xp) = skill_xp_progress(character, skill);
        let remaining_xp = xp_to_next_tier(level, xp, max_xp);
        if remaining_xp <= 0 { continue; }

        for item in &state.data.items {
            let Some(craft) = &item.craft else { continue };
            if craft.skill.as_deref() != Some(skill) { continue; }
            if craft.level.is_some_and(|req| level < req) { continue; }

            let xp_per_craft = calculate_crafting_xp(level, item, skill, character.wisdom);
            if xp_per_craft <= 0.0 { continue; }

            let crafts_needed = (remaining_xp as f64 / xp_per_craft).ceil() as i32;
            if crafts_needed > 0 {
                entries.push(CraftPlanEntry {
                    code: item.code.clone(),
                    skill: skill.to_string(),
                    quantity: crafts_needed,
                    tier: PlanTier::Wishlist,
                });
            }
        }
    }

    entries
}

/// Net per-unit ingredient need for one craft of `item` under `skill`, after subtracting the
/// expected recycling return (weaponcrafting/gearcrafting/jewelrycrafting only — a no-op
/// passthrough for alchemy, which is never recyclable). `f64`, un-rounded — callers round once
/// after multiplying by however many crafts they're projecting, not per unit.
fn net_ingredient_need(item: &Item, skill: &str) -> HashMap<String, f64> {
    let craft = item.craft.as_ref().unwrap();
    let returns = if is_recyclable_for_wishlist(item, skill) {
        recycle_returns_per_ingredient(craft)
    } else {
        HashMap::new()
    };
    craft.items.iter()
        .map(|ing| {
            let net = (ing.quantity as f64 - returns.get(&ing.code).copied().unwrap_or(0.0)).max(0.0);
            (ing.code.clone(), net)
        })
        .collect()
}

/// Splits wishlist `entries` into what's craftable right now against `remaining_supply` (net of
/// expected recycling returns) and what's reserved for later. Unlike `allocate_upgrade_crafts`'s
/// all-or-nothing equip demand, a wishlist entry is *partially* fillable: crafts as many as
/// `remaining_supply` allows (bounded by the scarcest net-of-recycling ingredient, clamped to the
/// entry's projected `crafts_needed`), and reserves the shortfall's direct ingredients for the
/// rest.
pub(crate) fn classify_wishlist_craftable(
    state: &GameState,
    character: &Character,
    entries: Vec<CraftPlanEntry>,
    remaining_supply: &mut HashMap<String, i32>,
) -> (Vec<CraftPlanEntry>, HashMap<String, i32>) {
    let mut craftable = Vec::new();
    let mut reserved: HashMap<String, i32> = HashMap::new();

    for entry in entries {
        let item  = match state.data.items.iter().find(|i| i.code == entry.code) { Some(i) => i, None => continue };
        let craft = match &item.craft { Some(c) => c, None => continue };
        if craft.level.is_some_and(|req| skill_level(character, &entry.skill) < req) { continue; } // defensive; build_wishlist already filters this

        let per_unit = net_ingredient_need(item, &entry.skill);

        let max_affordable = per_unit.iter()
            .filter(|(_, &qty)| qty > 0.0)
            .map(|(code, &qty)| {
                let have = remaining_supply.get(code).copied().unwrap_or(0) as f64;
                (have / qty).floor() as i32
            })
            .min()
            .unwrap_or(0)
            .clamp(0, entry.quantity);

        if max_affordable > 0 {
            for (code, qty) in &per_unit {
                let need = (qty * max_affordable as f64).ceil() as i32;
                *remaining_supply.entry(code.clone()).or_insert(0) -= need;
            }
            craftable.push(CraftPlanEntry {
                code: entry.code.clone(), skill: entry.skill.clone(), quantity: max_affordable, tier: PlanTier::Wishlist,
            });
        }

        let shortfall = entry.quantity - max_affordable;
        if shortfall > 0 {
            for (code, qty) in &per_unit {
                let need = (qty * shortfall as f64).ceil() as i32;
                *reserved.entry(code.clone()).or_insert(0) += need;
            }
        }
    }

    (craftable, reserved)
}

/// Weaponcrafting/gearcrafting/jewelrycrafting items (excluding anything already queued as an
/// upgrade or wishlist entry) the character can currently craft. Grouped by skill, lowest-level
/// skill first (so a lagging skill catches up instead of one skill racing ahead of the other two),
/// and within each skill ranked by XP per ingredient — the fallback once upgrade and wishlist
/// demand are satisfied.
pub(crate) fn filler_candidates(state: &GameState, character: &Character, exclude: &HashSet<String>) -> Vec<CraftPlanEntry> {
    let mut skills_by_level = CRAFTING_SKILLS;
    skills_by_level.sort_by_key(|&skill| skill_level(character, skill));

    let mut result = Vec::new();

    for skill in skills_by_level {
        let char_level = skill_level(character, skill);
        let mut candidates: Vec<(String, f64)> = Vec::new();

        for item in &state.data.items {
            if exclude.contains(&item.code) { continue; }
            let craft = match &item.craft { Some(c) => c, None => continue };
            if craft.skill.as_deref() != Some(skill) { continue; }
            if craft.level.is_some_and(|req| char_level < req) { continue; }

            let xp = craft_xp_per_ingredient(char_level, item, skill, character.wisdom);
            if xp > 0.0 {
                candidates.push((item.code.clone(), xp));
            }
        }

        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        result.extend(candidates.into_iter().map(|(code, _xp)| CraftPlanEntry {
            code, skill: skill.to_string(), quantity: i32::MAX, tier: PlanTier::Filler,
        }));
    }

    result
}

/// Runs the full upgrade → wishlist → filler priority pipeline against `remaining_supply`
/// (upgrades claim materials first, then wishlist gets whatever's left, matching "equipment
/// upgrades... higher priority"), merging both reservation maps into one and seeding
/// `state`'s reserved-materials cache. Returns the combined craftable entries (upgrade + wishlist,
/// pre-confirmed quantities) plus the exclusion set filler must avoid.
pub(crate) fn plan_crafting_priority(
    state: &GameState,
    character: &Character,
    remaining_supply: &mut HashMap<String, i32>,
) -> (Vec<CraftPlanEntry>, HashMap<String, i32>, HashSet<String>) {
    let (craftable_upgrades, upgrade_reserved, upgrade_codes) = allocate_upgrade_crafts(state, character, remaining_supply);

    let wishlist_entries = build_wishlist(state, character);
    let wishlist_codes: HashSet<String> = wishlist_entries.iter().map(|e| e.code.clone()).collect();
    let (craftable_wishlist, wishlist_reserved) = classify_wishlist_craftable(state, character, wishlist_entries, remaining_supply);

    let mut reserved = upgrade_reserved;
    for (code, qty) in wishlist_reserved {
        *reserved.entry(code).or_insert(0) += qty;
    }
    state.set_reserved_materials(reserved.clone());

    let mut planned = craftable_upgrades;
    planned.extend(craftable_wishlist);

    let exclude: HashSet<String> = upgrade_codes.union(&wishlist_codes).cloned().collect();

    (planned, reserved, exclude)
}

/// Prints a preview of what's currently craftable (weaponcrafting/gearcrafting/jewelrycrafting/
/// alchemy) for `character` given the bank's current contents — meant for program startup, right
/// after the bank has been fetched but before any character loop has run. Since no character has
/// reported item ratings yet at that point, the upgrade allocator is normally empty and this shows
/// wishlist/filler candidates only; it naturally starts reflecting real upgrade demand once loops
/// are running and ratings get cached. Ignores inventory capacity (no character is physically
/// holding anything yet) — purely a bank-vs-recipes availability check. Also seeds `state`'s
/// reserved-materials cache, so gatherers have a sensible baseline from their very first cycle
/// instead of starting with nothing reserved.
pub async fn print_initial_crafting_plan(state: &GameState, character: &Character) {
    let bank = state.bank_snapshot().await;
    let mut remaining: HashMap<String, i32> = HashMap::new();
    for b in &bank {
        *remaining.entry(b.code.clone()).or_insert(0) += b.quantity;
    }

    let (mut planned_entries, reserved, exclude) = plan_crafting_priority(state, character, &mut remaining);

    for entry in filler_candidates(state, character, &exclude) {
        let item  = match state.data.items.iter().find(|i| i.code == entry.code) { Some(i) => i, None => continue };
        let craft = match &item.craft { Some(c) => c, None => continue };

        let craftable = craft.items.iter()
            .map(|ing| remaining.get(&ing.code).copied().unwrap_or(0) / ing.quantity.max(1))
            .min().unwrap_or(0);
        if craftable <= 0 { continue; }

        for ing in &craft.items {
            *remaining.entry(ing.code.clone()).or_insert(0) -= ing.quantity * craftable;
        }
        planned_entries.push(CraftPlanEntry { code: entry.code, skill: entry.skill, quantity: craftable, tier: PlanTier::Filler });
    }

    let planned: Vec<(&str, i32)> = planned_entries.iter().map(|e| (e.code.as_str(), e.quantity)).collect();
    println!("[init] Initial crafting plan preview for {}:", character.name);
    print_plan(&character.name, &planned, &reserved);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::GameData;
    use crate::optimize::{RankedItem, SlotRating};
    use crate::types::{CraftIngredient, CraftInfo};

    fn blank_character() -> Character {
        Character {
            name: "test".into(), account: "test".into(), skin: "".into(),
            level: 1, xp: 0, max_xp: 0, gold: 0, speed: 0,
            mining_level: 1, mining_xp: 0, mining_max_xp: 0,
            woodcutting_level: 1, woodcutting_xp: 0, woodcutting_max_xp: 0,
            fishing_level: 1, fishing_xp: 0, fishing_max_xp: 0,
            weaponcrafting_level: 3, weaponcrafting_xp: 100, weaponcrafting_max_xp: 350,
            gearcrafting_level: 1, gearcrafting_xp: 0, gearcrafting_max_xp: 150,
            jewelrycrafting_level: 1, jewelrycrafting_xp: 0, jewelrycrafting_max_xp: 150,
            cooking_level: 1, cooking_xp: 0, cooking_max_xp: 0,
            alchemy_level: 1, alchemy_xp: 0, alchemy_max_xp: 150,
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

    fn make_craft_item(code: &str, level: i32, item_type: &str, skill: &str, ingredients: Vec<(&str, i32)>) -> Item {
        Item {
            name: code.into(), code: code.into(), level, item_type: item_type.into(),
            subtype: "".into(), description: "".into(), conditions: vec![],
            effects: vec![],
            craft: Some(CraftInfo {
                skill: Some(skill.into()),
                level: None,
                items: ingredients.into_iter().map(|(c, q)| CraftIngredient { code: c.into(), quantity: q }).collect(),
                quantity: 1,
            }),
            tradeable: true,
            recyclable: false,
        }
    }

    fn blank_data(items: Vec<Item>) -> GameData {
        GameData { monsters: vec![], items, resources: vec![], maps: vec![], craftable_equip: vec![] }
    }

    /// Two characters each want a different weapon, both needing the same scarce bar, with only
    /// enough for one. The higher-rated one (regardless of which character or slot it belongs to)
    /// should win the scarce material and get planned; the other should be reserved instead.
    #[test]
    fn allocate_upgrade_crafts_gives_scarce_materials_to_higher_rated_pick() {
        let items = vec![
            make_craft_item("good_sword", 5, "weapon", "weaponcrafting", vec![("bar", 5)]),
            make_craft_item("okay_sword", 5, "weapon", "weaponcrafting", vec![("bar", 5)]),
        ];
        let data = blank_data(items);
        let state = GameState::new(data);

        state.set_item_ratings("char_a", vec![SlotRating {
            slot: "weapon", category: "weapon",
            ranked: vec![RankedItem { code: "good_sword".into(), rating: 900.0 }],
        }], &blank_character());
        state.set_item_ratings("char_b", vec![SlotRating {
            slot: "weapon", category: "weapon",
            ranked: vec![RankedItem { code: "okay_sword".into(), rating: 100.0 }],
        }], &blank_character());

        let mut remaining_supply: HashMap<String, i32> = [("bar".to_string(), 5)].into_iter().collect();
        let crafter = blank_character(); // weaponcrafting_level 3, meets no crafting-level requirement here (recipe has none)

        let (planned, reserved, _codes) = allocate_upgrade_crafts(&state, &crafter, &mut remaining_supply);

        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].code, "good_sword");
        assert_eq!(reserved.get("bar").copied().unwrap_or(0), 5); // okay_sword's blocked demand reserved
    }

    /// Two alternative recipes that could each independently close the same skill's tier gap
    /// should BOTH appear on the wishlist (a union of every viable path), not just one.
    #[test]
    fn build_wishlist_unions_alternative_recipes_for_same_skill() {
        let items = vec![
            make_craft_item("lizard_dagger", 3, "weapon", "weaponcrafting", vec![("lizard_scale", 1)]),
            make_craft_item("mercury_dagger", 3, "weapon", "weaponcrafting", vec![("mercury_bar", 2)]),
        ];
        let data = blank_data(items);
        let state = GameState::new(data);

        let character = blank_character(); // weaponcrafting_level 3, xp 100/350

        let wishlist = build_wishlist(&state, &character);
        let codes: HashSet<&str> = wishlist.iter().map(|e| e.code.as_str()).collect();

        assert!(codes.contains("lizard_dagger"), "expected lizard_dagger on the wishlist, got {codes:?}");
        assert!(codes.contains("mercury_dagger"), "expected mercury_dagger on the wishlist, got {codes:?}");
        assert!(wishlist.iter().all(|e| e.tier == PlanTier::Wishlist));
        assert!(wishlist.iter().all(|e| e.quantity > 0));
    }
}
