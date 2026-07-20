use crate::api::GameData;
use crate::types::{Character, Item, Resource};

use super::brute_force::brute_force_optimal_loadout;
use super::combat_sim::best_achievable_xph;
use super::eligibility::{adjust_combat_stats, item_effect_value, meets_conditions};
use super::gathering_sim::gathering_xp_per_hour;
use super::skills::{refining_skill_for, skill_level};

// ── Item optimization ────────────────────────────────────────────────────────
//
// Ranks every equippable item for each of a character's gear slots by the
// XP/hour delta it would produce relative to that slot being empty, given the
// character's current role (fighting a specific monster, or gathering a
// specific resource). Items whose conditions the character doesn't meet are
// rated -1 (worse than empty); items whose only effects aren't modeled by any
// XP/hour formula in this codebase (threat, prospecting, gold, consumable
// buffs, etc.) tie with empty at 0.

#[derive(Clone, Copy)]
pub enum ItemRole<'a> {
    /// No specific target monster — combat ratings are always judged by the best-achievable
    /// XP/hour across *every* monster the character could guarantee beat with the candidate gear
    /// (see `best_achievable_xph`), so which monster ends up optimal is an output of the search,
    /// not an input to it.
    Combat,
    Gathering { skill: &'a str, resource: &'a Resource },
}

/// The equipment slots this optimizer considers part of the loadout: (label, item-type category,
/// currently-equipped code). Deliberately excludes utility1/utility2 (consumables, not persistent
/// gear) and bag — those are special-cased and handled separately elsewhere, not by this per-slot
/// XP/hour rating machinery.
pub(crate) fn slot_definitions(character: &Character) -> [(&'static str, &'static str, &str); 13] {
    [
        ("weapon",     "weapon",     character.weapon_slot.as_str()),
        ("shield",     "shield",     character.shield_slot.as_str()),
        ("helmet",     "helmet",     character.helmet_slot.as_str()),
        ("body_armor", "body_armor", character.body_armor_slot.as_str()),
        ("leg_armor",  "leg_armor",  character.leg_armor_slot.as_str()),
        ("boots",      "boots",      character.boots_slot.as_str()),
        ("ring1",      "ring",       character.ring1_slot.as_str()),
        ("ring2",      "ring",       character.ring2_slot.as_str()),
        ("amulet",     "amulet",     character.amulet_slot.as_str()),
        ("artifact1",  "artifact",   character.artifact1_slot.as_str()),
        ("artifact2",  "artifact",   character.artifact2_slot.as_str()),
        ("artifact3",  "artifact",   character.artifact3_slot.as_str()),
        ("rune",       "rune",       character.rune_slot.as_str()),
    ]
}

/// Every slot's currently-equipped item code, keyed by slot label — empty slots are omitted. Used
/// by `GameState` to avoid re-flagging a bank item a character already has equipped (a spare copy
/// left in the bank after equipping one is not something to keep chasing).
pub fn equipped_codes(character: &Character) -> std::collections::HashMap<String, String> {
    slot_definitions(character).into_iter()
        .filter(|(_, _, code)| !code.is_empty())
        .map(|(slot, _, code)| (slot.to_string(), code.to_string()))
        .collect()
}

pub(crate) fn current_item<'a>(data: &'a GameData, code: &str) -> Option<&'a Item> {
    if code.is_empty() { None } else { data.items.iter().find(|i| i.code == code) }
}

/// Total gathering-cooldown-reduction percentage currently contributed by all
/// equipped gear for `skill` (in practice only ever the weapon/tool slot).
pub(crate) fn total_gathering_cooldown_pct(character: &Character, data: &GameData, skill: &str) -> i32 {
    slot_definitions(character)
        .iter()
        .filter_map(|(_, _, code)| current_item(data, code))
        .map(|item| item_effect_value(item, skill))
        .sum()
}

/// XP/hour delta from equipping `candidate` in a slot currently holding `current` (None =
/// empty), measured as the change in the *best achievable* XP/hour across every monster the
/// character can guarantee beating — not just their current target. Offensive stats
/// (attack/dmg/crit/haste) show up here by speeding up the current best matchup; defensive
/// stats (HP/resistances) show up by potentially unlocking a stronger, previously-unsurvivable
/// monster with better XP/hour, even when they do nothing for the current fight. -1 if
/// conditions aren't met.
fn rate_combat_item(character: &Character, data: &GameData, current: Option<&Item>, candidate: &Item) -> f64 {
    if !meets_conditions(character, candidate) { return -1.0; }

    let bare = match current {
        Some(item) => adjust_combat_stats(character, item, -1),
        None       => character.clone(),
    };
    let baseline_xph = best_achievable_xph(&bare, data);
    let with_item     = adjust_combat_stats(&bare, candidate, 1);
    best_achievable_xph(&with_item, data) - baseline_xph
}

/// XP/hour delta from equipping `candidate` in a slot currently holding `current`
/// (None = empty), evaluated for `skill` against `resource`. -1 if conditions aren't met.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rate_gathering_item(
    character: &Character,
    resource: &Resource,
    skill: &str,
    total_cd: i32,
    current: Option<&Item>,
    candidate: &Item,
    data: &GameData,
) -> f64 {
    if !meets_conditions(character, candidate) { return -1.0; }

    let level = skill_level(character, skill);
    let refining_skill_level = skill_level(character, refining_skill_for(skill));
    let current_wisdom      = current.map_or(0, |i| item_effect_value(i, "wisdom"));
    let current_cd          = current.map_or(0, |i| item_effect_value(i, skill));
    let current_prospecting = current.map_or(0, |i| item_effect_value(i, "prospecting"));
    let bare_wisdom      = character.wisdom - current_wisdom;
    let bare_cd          = total_cd - current_cd;
    let bare_prospecting = character.prospecting - current_prospecting;

    let baseline_xph = gathering_xp_per_hour(level, resource, bare_wisdom, bare_cd, data, skill, refining_skill_level, bare_prospecting);
    let cand_wisdom      = bare_wisdom + item_effect_value(candidate, "wisdom");
    let cand_cd          = bare_cd + item_effect_value(candidate, skill);
    let cand_prospecting = bare_prospecting + item_effect_value(candidate, "prospecting");
    gathering_xp_per_hour(level, resource, cand_wisdom, cand_cd, data, skill, refining_skill_level, cand_prospecting) - baseline_xph
}

/// One item that rated strictly better than leaving its slot empty.
#[derive(Clone)]
pub struct RankedItem {
    pub code: String,
    pub rating: f64,
}

/// Every item that beats leaving a character slot empty, sorted descending by rating. Consumers
/// should walk this list in order and act on the first entry that's actually obtainable right
/// now (in the bank, currently craftable, ...) rather than only ever chasing `ranked[0]` — see
/// `GameState::flag_bank_upgrades` and `allocate_upgrade_crafts`, which do exactly that. An item rated no
/// better than empty (rating <= 0.0, including anything ineligible under the character's current
/// conditions) is never included, since there's no reason to ever prefer it over doing nothing.
#[derive(Clone)]
pub struct SlotRating {
    pub slot: &'static str,
    pub category: &'static str,
    pub ranked: Vec<RankedItem>,
}

impl SlotRating {
    pub fn best_code(&self) -> Option<&str> {
        self.ranked.first().map(|r| r.code.as_str())
    }

    pub fn best_rating(&self) -> f64 {
        self.ranked.first().map(|r| r.rating).unwrap_or(0.0)
    }
}

/// The rating (relative to leaving the slot empty) of whatever's *currently* equipped there,
/// according to `ranked` — used to tell whether some other candidate is a genuine upgrade over
/// current gear, not merely an upgrade over nothing (which `ranked` alone can't distinguish,
/// since it's built relative to empty). `f64::NEG_INFINITY` if the slot is genuinely empty right
/// now, so *any* ranked candidate — even one that only ties with empty — counts as a pickup;
/// `0.0` (the empty baseline) if something's equipped but isn't itself tracked in `ranked` (e.g.
/// ratings were computed before this character's gear last changed).
pub fn current_slot_rating(ranked: &[RankedItem], equipped_code: Option<&str>) -> f64 {
    match equipped_code {
        None => f64::NEG_INFINITY,
        Some(code) => ranked.iter().find(|item| item.code == code).map(|item| item.rating).unwrap_or(0.0),
    }
}

/// Ranks every equippable item for each of the character's gear slots against a gathering skill,
/// returning that ranking for callers that need to act on it (e.g. deciding which crafted items
/// are upgrades for which characters, or which slots still need a joint brute-force pass). Prints
/// nothing itself — for a gathering character this is only ever a partial picture (most gear has
/// no gathering formula at all, so many slots come up empty here and get filled in afterward by
/// `brute_force_optimal_loadout`'s combat-fallback search), so the caller is responsible for
/// printing the *complete* loadout once that fallback pass has actually run.
///
/// Combat characters don't use this at all anymore — `optimize_combat_loadout` replaces it there,
/// since combat is exactly the case where marginal (one-slot-at-a-time) analysis can miss upgrades
/// that only pay off through the combined effect of several equipped items, so it always needs the
/// full joint search rather than this single-pass ranking.
///
/// Synchronous and API-call-free: reuses the `Character` already fetched by
/// the caller's paired optimization call (find_optimal_gathering) instead of
/// fetching it again.
pub fn optimize_items(character: &Character, skill: &str, resource: &Resource, data: &GameData) -> Vec<SlotRating> {
    let total_cd = total_gathering_cooldown_pct(character, data, skill);

    let mut results = Vec::with_capacity(13);

    for (slot_label, category, current_code) in slot_definitions(character) {
        let current = current_item(data, current_code);

        let mut ranked: Vec<RankedItem> = data.items.iter()
            .filter(|i| i.item_type == category)
            .filter_map(|item| {
                let rating = rate_gathering_item(character, resource, skill, total_cd, current, item, data);
                (rating > 0.0).then(|| RankedItem { code: item.code.clone(), rating })
            })
            .collect();
        ranked.sort_by(|a, b| b.rating.partial_cmp(&a.rating).unwrap_or(std::cmp::Ordering::Equal));

        results.push(SlotRating { slot: slot_label, category, ranked });
    }

    results
}

/// Blank per-slot skeleton (no ranked alternatives yet) for every one of the character's 13
/// equipment slots — the starting point `brute_force_optimal_loadout` fills in for a combat
/// character, which has no prior marginal-analysis ranking to build on since it skips
/// `optimize_items` entirely in favor of jointly searching all 13 slots outright.
fn blank_ratings(character: &Character) -> Vec<SlotRating> {
    slot_definitions(character).into_iter()
        .map(|(slot, category, _)| SlotRating { slot, category, ranked: Vec::new() })
        .collect()
}

/// Every (slot, item) combat rating, unfiltered (including ineligible/-1 and tied/0 items) —
/// purely for the fight-loop character's debug CSV export, independent of the ranked-list-only
/// data the joint search itself produces.
fn full_marginal_ratings(character: &Character, data: &GameData) -> Vec<(&'static str, String, String, f64)> {
    let mut rows = Vec::new();
    for (slot_label, category, current_code) in slot_definitions(character) {
        let current = current_item(data, current_code);
        for item in data.items.iter().filter(|i| i.item_type == category) {
            rows.push((slot_label, item.code.clone(), item.name.clone(), rate_combat_item(character, data, current, item)));
        }
    }
    rows
}

/// Full joint-search item optimization for a combat-role character (the fight-loop character) —
/// this is the *only* item optimization combat characters get; there's no separate marginal pass
/// to run first, since `brute_force_optimal_loadout` always treats every slot as eligible for
/// combat and marginal (one-slot-at-a-time) analysis is exactly what misses upgrades that only pay
/// off through the combined effect of several equipped items. Takes no target monster — finding
/// the best-achievable monster for whatever gear the search settles on is part of what this does,
/// not a precondition for it; callers that also need a monster to move to/fight right now (with
/// *current* gear) should call `find_optimal_monster` separately.
pub fn optimize_combat_loadout(name: &str, character: &Character, data: &GameData) -> Vec<SlotRating> {
    write_full_ratings_csv(&full_marginal_ratings(character, data));

    brute_force_optimal_loadout(name, character, &ItemRole::Combat, data, &blank_ratings(character))
}

/// Writes every (slot, item, rating) combination considered for the fight-loop character to
/// `full_ratings_c1.csv`, overwriting it each time — a full dump for inspection/debugging,
/// beyond just the top pick per slot that gets printed to the console.
fn write_full_ratings_csv(rows: &[(&'static str, String, String, f64)]) {
    let mut csv = String::from("slot,item_code,item_name,rating\n");
    for (slot, code, item_name, rating) in rows {
        let escaped_name = item_name.replace('"', "\"\"");
        csv.push_str(&format!("{},{},\"{}\",{:.4}\n", slot, code, escaped_name, rating));
    }

    if let Err(e) = std::fs::write("full_ratings_c1.csv", csv) {
        eprintln!("Failed to write full_ratings_c1.csv: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ItemEffect, Monster};

    fn blank_character() -> Character {
        Character {
            name: "test".into(), account: "test".into(), skin: "".into(),
            level: 20, xp: 0, max_xp: 0, gold: 0, speed: 0,
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

    fn blank_monster() -> Monster {
        Monster {
            name: "test monster".into(), code: "test_monster".into(), level: 20,
            monster_type: "normal".into(), hp: 500,
            attack_fire: 0, attack_earth: 0, attack_water: 0, attack_air: 0,
            res_fire: 0, res_earth: 0, res_water: 0, res_air: 0,
            critical_strike: 0, initiative: 0, effects: vec![],
            min_gold: 0, max_gold: 0, drops: vec![],
        }
    }

    fn make_res_item(code: &str, item_type: &'static str, level: i32, res_code: &str, value: i32) -> Item {
        Item {
            name: code.into(), code: code.into(), level, item_type: item_type.into(),
            subtype: "".into(), description: "".into(), conditions: vec![],
            effects: vec![ItemEffect { code: res_code.into(), value, description: "".into() }],
            craft: None, tradeable: true, recyclable: false,
        }
    }

    /// Constructs a monster survivable only via the *combined* resistance from two different
    /// slots (amulet + helmet — both single-instance slots, so there's exactly one way to reach
    /// the required resistance, unlike ring1/ring2 which could double up on the same item),
    /// neither of which alone crosses the threshold — the exact blind spot in per-slot marginal
    /// analysis that motivated the joint brute-force search. Confirms `brute_force_optimal_loadout`
    /// finds the combination a single-slot pass would miss.
    #[test]
    fn brute_force_finds_combo_marginal_analysis_misses() {
        let mut character = blank_character();
        character.attack_fire = 1000; // one-shots the monster regardless of gear
        character.max_hp = 100;

        let mut monster = blank_monster();
        monster.hp = 1;
        // 140 fire dmg at 0% resistance = 140 (dies), at 20% (one item) = 112 (still dies),
        // at 40% (both items) = 84 (survives) — so surviving genuinely requires both items.
        monster.attack_fire = 140;
        monster.initiative = 1000; // monster always goes first, so its hit counts against threshold

        let data = GameData {
            monsters: vec![monster],
            items: vec![
                make_res_item("amulet_res20", "amulet", 20, "res_fire", 20),
                make_res_item("helmet_res20", "helmet", 20, "res_fire", 20),
            ],
            resources: vec![],
            maps: vec![],
            craftable_equip: vec![],
        };

        // Single-slot marginal analysis (each item rated alone, against the character's bare
        // stats) should find neither amulet nor helmet alone unlocks survivability against the
        // 140-damage hit — this is exactly what `rate_combat_item` computed internally for the
        // now-removed marginal `optimize_items` combat path.
        let amulet_item = data.items.iter().find(|i| i.code == "amulet_res20").unwrap();
        let helmet_item = data.items.iter().find(|i| i.code == "helmet_res20").unwrap();
        assert!(rate_combat_item(&character, &data, None, amulet_item) <= 0.0);
        assert!(rate_combat_item(&character, &data, None, helmet_item) <= 0.0);

        let joint = optimize_combat_loadout("test", &character, &data);
        let amulet_joint = joint.iter().find(|r| r.slot == "amulet").unwrap();
        let helmet_joint = joint.iter().find(|r| r.slot == "helmet").unwrap();

        assert_eq!(amulet_joint.best_code(), Some("amulet_res20"));
        assert_eq!(helmet_joint.best_code(), Some("helmet_res20"));

        std::fs::remove_file("full_ratings_c1.csv").ok();
    }

    /// A pure-defense item that doesn't change best-achievable XP/hour at all (ties exactly with
    /// leaving the slot empty — here because the character already wins by an enormous margin, so
    /// extra resistance can't unlock a new monster or speed up the current one) should still get
    /// worn, since it never actively hurts. This was a real bug: the search's strict `score >
    /// best_score` comparison meant a genuinely free stat boost (a shield) was left unequipped
    /// forever because it never registered as a *strict* improvement.
    #[test]
    fn brute_force_prefers_non_empty_on_exact_tie() {
        let mut character = blank_character();
        character.attack_fire = 1000; // one-shots the monster regardless of gear
        character.max_hp = 1000;      // enormous margin — no amount of extra resistance matters

        let monster = blank_monster(); // hp 500, deals 0 damage — win is never in doubt either way

        let data = GameData {
            monsters: vec![monster],
            items: vec![make_res_item("wooden_shield", "shield", 1, "res_fire", 10)],
            resources: vec![],
            maps: vec![],
            craftable_equip: vec![],
        };

        let joint = optimize_combat_loadout("test", &character, &data);
        let shield = joint.iter().find(|r| r.slot == "shield").unwrap();
        assert_eq!(shield.best_code(), Some("wooden_shield"));

        std::fs::remove_file("full_ratings_c1.csv").ok();
    }

    /// Utility slots (consumables) and the bag slot are handled separately elsewhere and should
    /// never be considered part of the loadout this optimizer rates.
    #[test]
    fn utility_and_bag_slots_are_excluded() {
        let character = blank_character();
        let data = GameData {
            monsters: vec![blank_monster()],
            items: vec![],
            resources: vec![],
            maps: vec![],
            craftable_equip: vec![],
        };

        let ratings = optimize_combat_loadout("test", &character, &data);
        assert_eq!(ratings.len(), 13);
        assert!(!ratings.iter().any(|r| matches!(r.slot, "utility1" | "utility2" | "bag")));

        std::fs::remove_file("full_ratings_c1.csv").ok();
    }

    /// A small item that ties *exactly* with empty against the specific baseline used to rank its
    /// slot (here: not enough HP on its own to cross a survivability threshold, unlike a bigger
    /// item that does) must still be tracked as a ranked fallback candidate, not dropped — this
    /// was a real bug: plain Copper Boots (a tiny, unconditional upgrade) disappeared from the
    /// ranked list entirely next to Iron Boots (a real, measurable upgrade), so when only Copper
    /// Boots was actually available in the bank, nothing ever got flagged.
    #[test]
    fn rank_touched_slots_keeps_zero_tied_items_as_fallback() {
        let mut character = blank_character();
        character.attack_fire = 1000; // one-shots the monster regardless of gear
        character.max_hp = 100;

        let mut monster = blank_monster();
        monster.hp = 1;
        monster.attack_fire = 120; // lethal at 100 or 105 hp; survivable at 150 hp
        monster.initiative = 1000; // monster always goes first

        let data = GameData {
            monsters: vec![monster],
            items: vec![
                make_res_item("big_boots", "boots", 10, "hp", 50),  // crosses the survivability threshold
                make_res_item("small_boots", "boots", 1, "hp", 5),  // doesn't -- ties exactly with empty
            ],
            resources: vec![],
            maps: vec![],
            craftable_equip: vec![],
        };

        let joint = optimize_combat_loadout("test", &character, &data);
        let boots = joint.iter().find(|r| r.slot == "boots").unwrap();

        assert_eq!(boots.best_code(), Some("big_boots"));
        let small = boots.ranked.iter().find(|r| r.code == "small_boots");
        assert!(small.is_some(), "small_boots should still be tracked as a zero-value fallback");
        assert_eq!(small.unwrap().rating, 0.0);

        std::fs::remove_file("full_ratings_c1.csv").ok();
    }
}
