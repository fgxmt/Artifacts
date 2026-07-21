use crate::api::GameData;
use crate::formulas::{calculate_cooldown, calculate_xp};
use crate::types::{Character, Monster};

// ── Damage helpers ───────────────────────────────────────────────────────────

struct BattleResult {
    won: bool,
    turns: i32,
}

fn element_damage(base_attack: i32, global_dmg: i32, elemental_dmg: i32, resistance: i32) -> f64 {
    let total_bonus = global_dmg + elemental_dmg;
    let elemental_attack = (base_attack as f64 * (1.0 + total_bonus as f64 / 100.0)).round();
    (elemental_attack * (1.0 - resistance as f64 / 100.0)).round()
}

fn char_base_damage(character: &Character, monster: &Monster) -> f64 {
    element_damage(character.attack_fire,  character.dmg, character.dmg_fire,  monster.res_fire)
    + element_damage(character.attack_earth, character.dmg, character.dmg_earth, monster.res_earth)
    + element_damage(character.attack_water, character.dmg, character.dmg_water, monster.res_water)
    + element_damage(character.attack_air,   character.dmg, character.dmg_air,   monster.res_air)
}

/// NOTE: monsters are assumed to have no global/elemental damage bonus (not in schema).
fn monster_base_damage(monster: &Monster, character: &Character) -> f64 {
    element_damage(monster.attack_fire,  0, 0, character.res_fire)
    + element_damage(monster.attack_earth, 0, 0, character.res_earth)
    + element_damage(monster.attack_water, 0, 0, character.res_water)
    + element_damage(monster.attack_air,   0, 0, character.res_air)
}

fn char_avg_damage(character: &Character, monster: &Monster) -> f64 {
    char_base_damage(character, monster) * (1.0 + 0.5 * character.critical_strike as f64 / 100.0)
}

fn monster_avg_damage(monster: &Monster, character: &Character) -> f64 {
    monster_base_damage(monster, character) * (1.0 + 0.5 * monster.critical_strike as f64 / 100.0)
}

/// Worst-case: crit 0% for character unless critical_strike ≥ 100.
fn char_worst_damage(character: &Character, monster: &Monster) -> f64 {
    let base = char_base_damage(character, monster);
    if character.critical_strike >= 100 { base * 1.5 } else { base }
}

/// Worst-case: crit 100% for monster unless critical_strike ≤ 0.
fn monster_worst_damage(monster: &Monster, character: &Character) -> f64 {
    let base = monster_base_damage(monster, character);
    if monster.critical_strike <= 0 { base } else { base * 1.5 }
}

// ── Health-potion healing ────────────────────────────────────────────────────
//
// A (non-splash) health potion equipped in a utility slot auto-triggers each round it's still
// stocked: "Restores N HP at the start of the turn if the player has lost more than 50% of their
// health points." This only models that one effect code (`restore`) — other utility effects
// (damage/resistance boosts, antidotes, splash healing for allies, teleports, ...) aren't combat-
// simulated here, same as before.

#[derive(Clone)]
struct RestorePotion {
    value: i32,
    quantity: i32,
}

/// Every equipped utility-slot item with a `restore` effect and remaining charges, resolved from
/// `character`'s utility slots against `data`'s item list.
fn restore_potions(character: &Character, data: &GameData) -> Vec<RestorePotion> {
    [
        (character.utility1_slot.as_str(), character.utility1_slot_quantity),
        (character.utility2_slot.as_str(), character.utility2_slot_quantity),
    ]
    .into_iter()
    .filter(|(code, qty)| !code.is_empty() && *qty > 0)
    .filter_map(|(code, qty)| {
        let item = data.items.iter().find(|i| i.code == code)?;
        let value = item.effects.iter().find(|e| e.code == "restore")?.value;
        Some(RestorePotion { value, quantity: qty })
    })
    .collect()
}

/// Start-of-round healing: every potion with charges remaining procs once `hp` has dropped below
/// 50% of `max_hp`, each restoring its value (capped at `max_hp`) and consuming one charge.
fn apply_restore_potions(hp: f64, max_hp: f64, potions: &mut [RestorePotion]) -> f64 {
    if hp >= max_hp / 2.0 { return hp; }
    let mut hp = hp;
    for p in potions.iter_mut() {
        if p.quantity > 0 {
            hp = (hp + p.value as f64).min(max_hp);
            p.quantity -= 1;
        }
    }
    hp
}

// ── Turn-order helpers ───────────────────────────────────────────────────────

/// None = exact tie on both initiative and HP.
fn turn_order(character_initiative: i32, character_hp: i32, monster: &Monster) -> Option<bool> {
    match character_initiative.cmp(&monster.initiative) {
        std::cmp::Ordering::Greater => Some(true),
        std::cmp::Ordering::Less    => Some(false),
        std::cmp::Ordering::Equal   => match character_hp.cmp(&monster.hp) {
            std::cmp::Ordering::Greater => Some(true),
            std::cmp::Ordering::Less    => Some(false),
            std::cmp::Ordering::Equal   => None,
        },
    }
}

// ── Core battle simulation ───────────────────────────────────────────────────

fn simulate_battle(
    char_hp: f64,
    max_hp: f64,
    char_dmg: f64,
    monster_hp: f64,
    monster_dmg: f64,
    char_first: bool,
    potions: &[RestorePotion],
) -> BattleResult {
    let mut char_hp = char_hp;
    let mut monster_hp = monster_hp;
    let mut potions: Vec<RestorePotion> = potions.to_vec();

    for round in 1..=100 {
        char_hp = apply_restore_potions(char_hp, max_hp, &mut potions);
        if char_first {
            monster_hp -= char_dmg;
            if monster_hp <= 0.0 { return BattleResult { won: true, turns: round }; }
            char_hp -= monster_dmg;
            if char_hp <= 0.0 { return BattleResult { won: false, turns: round }; }
        } else {
            char_hp -= monster_dmg;
            if char_hp <= 0.0 { return BattleResult { won: false, turns: round }; }
            monster_hp -= char_dmg;
            if monster_hp <= 0.0 { return BattleResult { won: true, turns: round }; }
        }
    }

    BattleResult { won: false, turns: 100 }
}

// ── Per-monster evaluations ──────────────────────────────────────────────────

fn xp_per_hour(xp: f64, turns: i32, haste: i32) -> f64 {
    let cooldown = calculate_cooldown(turns, haste);
    if cooldown <= 0.0 { return 0.0; }
    xp / (cooldown / 3600.0)
}

pub(crate) fn evaluate_average(character: &Character, monster: &Monster, data: &GameData) -> f64 {
    let char_dmg = char_avg_damage(character, monster);
    let mon_dmg  = monster_avg_damage(monster, character);
    let xp       = calculate_xp(character, monster);
    let potions  = restore_potions(character, data);

    let sim = |char_first: bool| -> f64 {
        let r = simulate_battle(
            character.max_hp as f64, character.max_hp as f64, char_dmg,
            monster.hp as f64,       mon_dmg,
            char_first, &potions,
        );
        if r.won { xp_per_hour(xp, r.turns, character.haste) } else { 0.0 }
    };

    match turn_order(character.initiative, character.max_hp, monster) {
        Some(char_first) => sim(char_first),
        None => 0.5 * sim(true) + 0.5 * sim(false),
    }
}

/// Closed-form worst-case threshold with no health potions in the mix: tracks cumulative
/// worst-case damage taken and derives the minimum starting HP that would have survived it,
/// instead of testing a fixed starting HP for win/loss. Kept as its own exact path (rather than
/// folded into `survives_from`'s binary search) so the no-potion case — overwhelmingly the common
/// one — is unchanged from before health potions existed.
fn minimum_hp_threshold_no_potions(char_dmg: f64, mon_dmg: f64, monster_hp: f64, char_first: bool) -> Option<i32> {
    let mut monster_hp = monster_hp;
    let mut damage_taken = 0.0_f64;

    for _round in 1..=100 {
        if char_first {
            monster_hp -= char_dmg;
            if monster_hp <= 0.0 { return Some((damage_taken + 1.0).ceil() as i32); }
            damage_taken += mon_dmg;
        } else {
            damage_taken += mon_dmg;
            monster_hp -= char_dmg;
            if monster_hp <= 0.0 { return Some((damage_taken + 1.0).ceil() as i32); }
        }
    }

    None
}

/// Worst-case survival check: starting this fight at `start_hp` (out of `max_hp`), does the
/// character kill `monster` before dying, under worst-case damage every round and any equipped
/// health potions' start-of-round auto-heal?
fn survives_from(start_hp: i32, max_hp: i32, char_dmg: f64, mon_dmg: f64, monster_hp: f64, char_first: bool, potions: &[RestorePotion]) -> bool {
    let mut char_hp = start_hp as f64;
    let mut monster_hp = monster_hp;
    let mut potions: Vec<RestorePotion> = potions.to_vec();

    for _round in 1..=100 {
        char_hp = apply_restore_potions(char_hp, max_hp as f64, &mut potions);
        if char_first {
            monster_hp -= char_dmg;
            if monster_hp <= 0.0 { return true; }
            char_hp -= mon_dmg;
            if char_hp <= 0.0 { return false; }
        } else {
            char_hp -= mon_dmg;
            if char_hp <= 0.0 { return false; }
            monster_hp -= char_dmg;
            if monster_hp <= 0.0 { return true; }
        }
    }

    false
}

/// The minimum HP `character` needs to guarantee beating `monster`, assuming worst-case damage
/// every round (character never crits unless guaranteed to, monster always crits unless
/// guaranteed not to) and any equipped health potions' auto-heal. `None` if the character can't
/// damage the monster at all, or the fight would run past the 100-round cap even at full HP.
///
/// With no potions equipped this is the exact closed-form calculation (cumulative damage taken
/// has a single fixed total, so the minimum starting HP is a direct formula). With potions,
/// whether a heal triggers depends on HP relative to `max_hp` at that moment, which itself depends
/// on starting HP — no more direct formula, so this binary-searches the minimum starting HP (in
/// `[1, max_hp]`) that survives a forward simulation. More starting HP only ever delays when the
/// 50%-loss heal threshold is crossed, never causing extra damage or fewer heals, so survival is
/// monotonic in starting HP and the search is sound.
pub(crate) fn minimum_hp_threshold(character: &Character, monster: &Monster, data: &GameData) -> Option<i32> {
    let char_dmg = char_worst_damage(character, monster);
    let mon_dmg  = monster_worst_damage(monster, character);

    if char_dmg <= 0.0 { return None; }

    let char_first = turn_order(character.initiative, character.max_hp, monster).unwrap_or_default();
    let potions = restore_potions(character, data);

    if potions.is_empty() {
        return minimum_hp_threshold_no_potions(char_dmg, mon_dmg, monster.hp as f64, char_first);
    }

    let max_hp = character.max_hp;
    if !survives_from(max_hp, max_hp, char_dmg, mon_dmg, monster.hp as f64, char_first, &potions) {
        return None;
    }

    let mut lo = 1;
    let mut hi = max_hp;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if survives_from(mid, max_hp, char_dmg, mon_dmg, monster.hp as f64, char_first, &potions) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(lo)
}

/// The best (monster, XP/hour) pair among monsters `character` can guarantee beating — the same
/// selection `find_optimal_monster` makes, but synchronous (no `get_character` fetch) and silent
/// (no per-monster table print).
fn best_fightable<'a>(character: &Character, data: &'a GameData) -> Option<(&'a Monster, f64)> {
    data.monsters.iter()
        .filter_map(|m| {
            let threshold = minimum_hp_threshold(character, m, data)?;
            (threshold <= character.max_hp).then(|| (m, evaluate_average(character, m, data)))
        })
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

/// The highest XP/hour `character` can achieve against any monster they can guarantee beating —
/// 0.0 if none. Unlike evaluating a single fixed monster, this correctly credits defensive stats
/// (HP, resistances) that don't speed up killing the *current* target but unlock a stronger,
/// previously-unsurvivable monster with a better XP/hour.
pub(crate) fn best_achievable_xph(character: &Character, data: &GameData) -> f64 {
    best_fightable(character, data).map(|(_, xph)| xph).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Item, ItemEffect};

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

    fn blank_data(items: Vec<Item>) -> GameData {
        GameData { monsters: vec![], items, resources: vec![], maps: vec![], craftable_equip: vec![] }
    }

    fn health_potion_item(code: &str, restore: i32) -> Item {
        Item {
            name: code.into(), code: code.into(), level: 1, item_type: "utility".into(),
            subtype: "potion".into(), description: "".into(), conditions: vec![],
            effects: vec![ItemEffect { code: "restore".into(), value: restore, description: "".into() }],
            craft: None, tradeable: true, recyclable: false,
        }
    }

    /// Old closed-form implementation, kept here only to cross-check the new turn-by-turn
    /// simulation produces identical results (worst-case damage is constant per round, so the
    /// two approaches should always agree exactly).
    fn minimum_hp_threshold_closed_form(character: &Character, monster: &Monster) -> Option<i32> {
        let char_dmg = char_worst_damage(character, monster);
        let mon_dmg  = monster_worst_damage(monster, character);
        if char_dmg <= 0.0 { return None; }
        let char_first = turn_order(character.initiative, character.max_hp, monster).unwrap_or_default();
        let rounds_to_kill = (monster.hp as f64 / char_dmg).ceil() as i32;
        if rounds_to_kill > 100 { return None; }
        let monster_hits = if char_first { rounds_to_kill - 1 } else { rounds_to_kill };
        let damage_taken = monster_hits as f64 * mon_dmg;
        Some((damage_taken + 1.0).ceil() as i32)
    }

    #[test]
    fn turn_by_turn_matches_closed_form_across_scenarios() {
        let scenarios: Vec<(i32, i32, i32, i32, i32)> = vec![
            // (char attack_fire, char initiative, monster hp, monster attack_fire, monster initiative)
            (500, 100, 500, 10, 0),   // character much faster, kills in 1 hit
            (100, 100, 500, 10, 0),   // several rounds, character first
            (100, 0, 500, 10, 100),   // several rounds, monster first
            (100, 0, 500, 10, 0),     // exact tie -> monster-first pessimistic default
            (17, 50, 1000, 25, 50),   // long fight, near the 100-round edge
            (0, 100, 500, 10, 0),     // character deals no damage -> unwinnable
        ];

        for (atk, init, hp, matk, minit) in scenarios {
            let mut character = blank_character();
            character.attack_fire = atk;
            character.initiative = init;

            let mut monster = blank_monster();
            monster.hp = hp;
            monster.attack_fire = matk;
            monster.initiative = minit;

            let expected = minimum_hp_threshold_closed_form(&character, &monster);
            let actual   = minimum_hp_threshold(&character, &monster, &blank_data(vec![]));
            assert_eq!(actual, expected, "mismatch for atk={atk} init={init} hp={hp} matk={matk} minit={minit}");
        }
    }

    #[test]
    fn turn_by_turn_gives_up_past_100_rounds() {
        let mut character = blank_character();
        character.attack_fire = 1; // negligible damage vs a huge HP pool
        let mut monster = blank_monster();
        monster.hp = 100_000;
        monster.attack_fire = 5;
        assert_eq!(minimum_hp_threshold(&character, &monster, &blank_data(vec![])), None);
    }

    /// A monster that's unsurvivable with no healing becomes survivable once a health potion is
    /// equipped in a utility slot — the whole point of modeling `restore` in the simulator.
    #[test]
    fn health_potion_unlocks_otherwise_unsurvivable_monster() {
        let mut character = blank_character();
        character.attack_fire = 26; // kills a 500hp monster in ~20 rounds
        character.max_hp = 100;
        character.utility1_slot = "small_health_potion".into();
        character.utility1_slot_quantity = 10;

        let mut monster = blank_monster();
        monster.hp = 500;
        monster.attack_fire = 15; // lethal cumulative damage without healing
        monster.initiative = 100; // monster always goes first

        let data = blank_data(vec![health_potion_item("small_health_potion", 30)]);

        let without_potion = {
            let mut bare = character.clone();
            bare.utility1_slot = "".into();
            bare.utility1_slot_quantity = 0;
            minimum_hp_threshold(&bare, &monster, &data)
        };
        let with_potion = minimum_hp_threshold(&character, &monster, &data);

        assert!(with_potion.is_some(), "expected the potion to make this monster survivable");
        assert!(
            with_potion.unwrap() < without_potion.unwrap_or(i32::MAX),
            "potion should lower (or unlock) the survivable HP threshold: with={:?} without={:?}",
            with_potion, without_potion,
        );
    }

    /// A potion only heals when HP has dropped below 50% of max, restores the right amount capped
    /// at max_hp, and consumes exactly one charge per proc — then stops healing once charges run
    /// out, so the simulator never credits infinite free healing from a finite stock.
    #[test]
    fn apply_restore_potions_respects_threshold_cap_and_charges() {
        let mut potions = vec![RestorePotion { value: 30, quantity: 2 }];

        // At/above 50% of max_hp (100) -> no heal, no charge spent.
        assert_eq!(apply_restore_potions(60.0, 100.0, &mut potions), 60.0);
        assert_eq!(potions[0].quantity, 2);

        // Below 50% -> heals by the potion's value and consumes a charge.
        assert_eq!(apply_restore_potions(10.0, 100.0, &mut potions), 40.0);
        assert_eq!(potions[0].quantity, 1);

        // Healing is capped at max_hp even if the raw restore would overshoot it.
        assert_eq!(apply_restore_potions(15.0, 40.0, &mut potions), 40.0);
        assert_eq!(potions[0].quantity, 0);

        // Out of charges -> no more healing, even while still below the threshold.
        assert_eq!(apply_restore_potions(10.0, 100.0, &mut potions), 10.0);
    }
}
