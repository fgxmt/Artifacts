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
    char_dmg: f64,
    monster_hp: f64,
    monster_dmg: f64,
    char_first: bool,
) -> BattleResult {
    let mut char_hp = char_hp;
    let mut monster_hp = monster_hp;

    for round in 1..=100 {
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

pub(crate) fn evaluate_average(character: &Character, monster: &Monster) -> f64 {
    let char_dmg = char_avg_damage(character, monster);
    let mon_dmg  = monster_avg_damage(monster, character);
    let xp       = calculate_xp(character, monster);

    let sim = |char_first: bool| -> f64 {
        let r = simulate_battle(
            character.max_hp as f64, char_dmg,
            monster.hp as f64,       mon_dmg,
            char_first,
        );
        if r.won { xp_per_hour(xp, r.turns, character.haste) } else { 0.0 }
    };

    match turn_order(character.initiative, character.max_hp, monster) {
        Some(char_first) => sim(char_first),
        None => 0.5 * sim(true) + 0.5 * sim(false),
    }
}

/// Turn-by-turn worst-case simulation (mirrors `simulate_battle`'s round loop, but instead of
/// testing a fixed starting HP for win/loss, it tracks cumulative worst-case damage taken and
/// derives the minimum starting HP that would have survived it) — the minimum HP `character`
/// needs to guarantee beating `monster`, assuming worst-case damage every round (character never
/// crits unless guaranteed to, monster always crits unless guaranteed not to). `None` if the
/// character can't damage the monster at all, or the fight would run past the 100-round cap.
pub(crate) fn minimum_hp_threshold(character: &Character, monster: &Monster) -> Option<i32> {
    let char_dmg = char_worst_damage(character, monster);
    let mon_dmg  = monster_worst_damage(monster, character);

    if char_dmg <= 0.0 { return None; }

    let char_first = turn_order(character.initiative, character.max_hp, monster).unwrap_or_default();

    let mut monster_hp    = monster.hp as f64;
    let mut damage_taken  = 0.0_f64;

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

/// The best (monster, XP/hour) pair among monsters `character` can guarantee beating — the same
/// selection `find_optimal_monster` makes, but synchronous (no `get_character` fetch) and silent
/// (no per-monster table print).
fn best_fightable<'a>(character: &Character, data: &'a GameData) -> Option<(&'a Monster, f64)> {
    data.monsters.iter()
        .filter_map(|m| {
            let threshold = minimum_hp_threshold(character, m)?;
            (threshold <= character.max_hp).then(|| (m, evaluate_average(character, m)))
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
            let actual   = minimum_hp_threshold(&character, &monster);
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
        assert_eq!(minimum_hp_threshold(&character, &monster), None);
    }
}
