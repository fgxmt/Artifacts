use std::collections::HashSet;

use reqwest::Client;

use crate::api::{get_character, GameData};
use crate::formulas::craft_xp_per_ingredient;
use crate::types::{Character, Item, Map, MonsterDrop, Resource};

use super::combat_sim::{evaluate_average, minimum_hp_threshold};
use super::gathering_sim::{average_drop_yield, gathering_xp_per_hour};
use super::skills::{refining_skill_for, skill_level};
use super::slot_rating::total_gathering_cooldown_pct;

// ── Public result types ──────────────────────────────────────────────────────

pub struct MonsterEvaluation {
    pub name: String,
    pub code: String,
    pub avg_xp_per_hour: f64,
    pub min_hp_threshold: Option<i32>,
}

pub struct MonsterTarget {
    pub code: String,
    pub name: String,
    pub avg_xp_per_hour: f64,
    pub min_hp_threshold: i32,
    pub locations: Vec<(String, i32, i32)>,
}

#[derive(Clone)]
pub struct GatherTarget {
    pub code: String,
    pub name: String,
    pub avg_xp_per_hour: f64,
    pub locations: Vec<(String, i32, i32)>,
    pub drops: Vec<MonsterDrop>,
}

// ── Map helpers ──────────────────────────────────────────────────────────────

pub fn locations_raw_for(maps: &[Map], content_type: &str, code: &str) -> Vec<(String, i32, i32)> {
    maps.iter()
        .filter(|m| {
            m.interactions
                .as_ref()
                .and_then(|i| i.content.as_ref())
                .is_some_and(|c| c.content_type == content_type && c.code == code)
        })
        .map(|m| (m.layer.clone(), m.x, m.y))
        .collect()
}

fn locations_for(maps: &[Map], content_type: &str, code: &str) -> String {
    let locs = locations_raw_for(maps, content_type, code);
    if locs.is_empty() {
        "-".to_string()
    } else {
        locs.iter()
            .map(|(l, x, y)| format!("({},{},{})", l, x, y))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// ── Public optimize functions ────────────────────────────────────────────────

pub async fn find_optimal_monster(
    client: &Client,
    name: &str,
    data: &GameData,
) -> crate::types::Result<Option<MonsterTarget>> {
    let character = get_character(client, name).await?;
    let monsters  = &data.monsters;
    let maps      = &data.maps;

    println!("[{}] Evaluating {} monsters...", crate::ts_char(name), monsters.len());

    let mut evals: Vec<MonsterEvaluation> = monsters
        .iter()
        .map(|m| MonsterEvaluation {
            name:             m.name.clone(),
            code:             m.code.clone(),
            avg_xp_per_hour:  evaluate_average(&character, m, data),
            min_hp_threshold: minimum_hp_threshold(&character, m, data),
        })
        .collect();

    evals.sort_by(|a, b| {
        b.avg_xp_per_hour
            .partial_cmp(&a.avg_xp_per_hour)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    println!(
        "[{}] {:<30} {:>12}  {:<25}  Locations",
        crate::ts_char(name), "Monster", "XP/hr (avg)", "Guaranteed win (min HP)"
    );
    println!("[{}] {}", crate::ts_char(name), "─".repeat(85));

    for eval in &evals {
        let guarantee = match eval.min_hp_threshold {
            Some(hp) if hp <= character.max_hp => format!("{} HP", hp),
            _                                  => "Cannot guarantee".to_string(),
        };
        let locs = locations_for(maps, "monster", &eval.code);
        println!(
            "[{}] {:<30} {:>12.0}  {:<25}  {}",
            crate::ts_char(name), eval.name, eval.avg_xp_per_hour, guarantee, locs
        );
    }

    println!("[{}]", crate::ts_char(name));

    let optimal_eval = evals
        .iter()
        .filter(|e| e.min_hp_threshold.is_some_and(|hp| hp <= character.max_hp))
        .max_by(|a, b| {
            a.avg_xp_per_hour
                .partial_cmp(&b.avg_xp_per_hour)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

    let target = optimal_eval.map(|e| {
        let locs = locations_raw_for(maps, "monster", &e.code);
        let threshold = e.min_hp_threshold.unwrap();
        println!(
            "[{}] Optimal: {} — {:.0} XP/hr (guaranteed win from {} HP)  {}",
            crate::ts_char(name),
            e.name,
            e.avg_xp_per_hour,
            threshold,
            locations_for(maps, "monster", &e.code),
        );
        MonsterTarget {
            code: e.code.clone(),
            name: e.name.clone(),
            avg_xp_per_hour: e.avg_xp_per_hour,
            min_hp_threshold: threshold,
            locations: locs,
        }
    });

    if target.is_none() {
        println!(
            "[{}] No monster can be guaranteed to beat at current stats.",
            crate::ts_char(name)
        );
    }

    Ok(target)
}

/// Among monsters `character` can guarantee-beat (worst-case damage every round, same "guaranteed
/// win" bar as `find_optimal_monster`), the one with the best expected yield of any item in
/// `material_codes` — used to prioritize farming monsters that drop wishlist crafting materials
/// over plain combat XP/hour once a fighting character's own level has outpaced crafting demand
/// (see `loops::repositioning`). Ranked by `average_drop_yield` (rate- and prospecting-aware, the
/// same formula gathering targets are ranked by), taking each monster's single best-matching drop
/// rather than summing multiple matches. `None` if `material_codes` is empty or no
/// guaranteed-beatable monster drops any of them, so the caller can fall back to
/// `find_optimal_monster`.
pub(crate) fn find_best_material_drop_target(
    character: &Character,
    data: &GameData,
    material_codes: &HashSet<String>,
) -> Option<MonsterTarget> {
    if material_codes.is_empty() { return None; }

    data.monsters.iter()
        .filter_map(|m| {
            let threshold = minimum_hp_threshold(character, m, data)?;
            if threshold > character.max_hp { return None; }

            let best_yield = m.drops.iter()
                .filter(|d| material_codes.contains(&d.code))
                .map(|d| average_drop_yield(d, character.prospecting))
                .fold(0.0_f64, f64::max);

            (best_yield > 0.0).then_some((m, threshold, best_yield))
        })
        .max_by(|(_, _, a), (_, _, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(m, threshold, _)| MonsterTarget {
            code: m.code.clone(),
            name: m.name.clone(),
            avg_xp_per_hour: evaluate_average(character, m, data),
            min_hp_threshold: threshold,
            locations: locations_raw_for(&data.maps, "monster", &m.code),
        })
}

/// Gathering-only skills: ranks resources at or below the character's skill level by XP/hour.
/// Returns all sources tied for the highest XP/hr.
pub async fn find_optimal_gathering(
    client: &Client,
    name: &str,
    skill: &str,
    data: &GameData,
) -> crate::types::Result<Vec<GatherTarget>> {
    let character  = get_character(client, name).await?;
    let resources  = &data.resources;
    let maps       = &data.maps;
    let char_level = skill_level(&character, skill);
    let refining_skill_level = skill_level(&character, refining_skill_for(skill));
    let cooldown_pct = total_gathering_cooldown_pct(&character, data, skill);

    // Routed through the shared gathering_xp_per_hour (rather than a separate inline formula) so
    // resource selection reflects the same refining-aware, gear-aware XP/hour that gear ratings
    // use — a resource with a great refining outlet can be optimal even if its raw-gather XP/hour
    // alone wouldn't be.
    let mut candidates: Vec<(&Resource, f64)> = resources
        .iter()
        .filter(|r| r.skill == skill && r.level <= char_level)
        .map(|r| {
            let xph = gathering_xp_per_hour(char_level, r, character.wisdom, cooldown_pct, data, skill, refining_skill_level, character.prospecting);
            (r, xph)
        })
        .collect();

    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    println!(
        "[{}] {} gathering — char level {}",
        crate::ts_char(name), skill, char_level
    );
    println!(
        "[{}] {:<30} {:>6}  {:>10}  Locations",
        crate::ts_char(name), "Resource", "Level", "XP/hr"
    );
    println!("[{}] {}", crate::ts_char(name), "─".repeat(70));

    for (r, xph) in &candidates {
        let locs = locations_for(maps, "resource", &r.code);
        println!(
            "[{}] {:<30} {:>6}  {:>10.0}  {}",
            crate::ts_char(name), r.name, r.level, xph, locs
        );
    }

    let best_xph = candidates.first().map(|(_, xph)| *xph).unwrap_or(0.0);

    let optimal_targets: Vec<GatherTarget> = candidates
        .iter()
        .filter(|(_, xph)| (xph - best_xph).abs() < 0.001)
        .map(|(r, xph)| {
            let locs = locations_raw_for(maps, "resource", &r.code);
            GatherTarget {
                code:            r.code.clone(),
                name:            r.name.clone(),
                avg_xp_per_hour: *xph,
                locations:       locs,
                drops:           r.drops.clone(),
            }
        })
        .collect();

    if optimal_targets.is_empty() {
        println!(
            "[{}] No {} sources available at skill level {}.",
            crate::ts_char(name), skill, char_level
        );
    } else {
        let names: Vec<&str> = optimal_targets.iter().map(|t| t.name.as_str()).collect();
        let locs_str: Vec<String> = optimal_targets
            .iter()
            .flat_map(|t| &t.locations)
            .map(|(l, x, y)| format!("({},{},{})", l, x, y))
            .collect();
        println!(
            "[{}] Optimal: {} — {:.0} XP/hr  {}",
            crate::ts_char(name),
            names.join(" / "),
            best_xph,
            locs_str.join(" "),
        );
    }

    Ok(optimal_targets)
}

/// Crafting-only skills: ranks craftable items by XP per ingredient consumed.
pub async fn find_optimal_crafting(
    client: &Client,
    name: &str,
    skill: &str,
    data: &GameData,
) -> crate::types::Result<()> {
    let character  = get_character(client, name).await?;
    let items      = &data.items;
    let char_level = skill_level(&character, skill);

    let mut candidates: Vec<(&Item, f64)> = items
        .iter()
        .filter(|i| i.craft.as_ref().is_some_and(|c| c.skill.as_deref() == Some(skill)))
        .map(|i| {
            let xp_per_ingredient = craft_xp_per_ingredient(char_level, i, skill, character.wisdom);
            (i, xp_per_ingredient)
        })
        .filter(|(_, xp)| *xp > 0.0)
        .collect();

    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    println!(
        "[{}] {} crafting — char level {}",
        crate::ts_char(name), skill, char_level
    );
    println!(
        "[{}] {:<30} {:>6}  {:>14}",
        crate::ts_char(name), "Item", "Level", "XP/ingredient"
    );
    println!("[{}] {}", crate::ts_char(name), "─".repeat(55));

    for (item, xp) in &candidates {
        println!(
            "[{}] {:<30} {:>6}  {:>14.2}",
            crate::ts_char(name), item.name, item.level, xp
        );
    }

    if let Some((item, xp)) = candidates.first() {
        println!(
            "[{}] Optimal: {} — {:.2} XP/ingredient",
            crate::ts_char(name), item.name, xp
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Monster;

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
            attack_fire: 1000, attack_earth: 0, attack_water: 0, attack_air: 0,
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

    fn blank_monster(code: &str, hp: i32) -> Monster {
        Monster {
            name: code.into(), code: code.into(), level: 1,
            monster_type: "normal".into(), hp,
            attack_fire: 0, attack_earth: 0, attack_water: 0, attack_air: 0,
            res_fire: 0, res_earth: 0, res_water: 0, res_air: 0,
            critical_strike: 0, initiative: 0, effects: vec![],
            min_gold: 0, max_gold: 0, drops: vec![],
        }
    }

    fn blank_data(monsters: Vec<Monster>) -> GameData {
        GameData { monsters, items: vec![], resources: vec![], maps: vec![], craftable_equip: vec![] }
    }

    #[test]
    fn find_best_material_drop_target_ignores_monsters_that_cant_be_beaten() {
        let mut too_tough = blank_monster("too_tough", 10_000); // enough HP that 1 hit doesn't end it
        too_tough.attack_fire = 1_000_000; // guaranteed-lethal hit
        too_tough.initiative = 1_000; // guaranteed to go first
        too_tough.drops = vec![MonsterDrop { code: "ore".into(), rate: 1, min_quantity: 1, max_quantity: 1 }];

        let mut beatable = blank_monster("beatable", 1);
        beatable.drops = vec![MonsterDrop { code: "ore".into(), rate: 5, min_quantity: 1, max_quantity: 1 }];

        let data = blank_data(vec![too_tough, beatable]);
        let character = blank_character();
        let materials: HashSet<String> = ["ore".to_string()].into_iter().collect();

        let target = find_best_material_drop_target(&character, &data, &materials);
        assert_eq!(target.map(|t| t.code), Some("beatable".to_string()));
    }

    #[test]
    fn find_best_material_drop_target_prefers_the_better_drop_rate() {
        let mut common = blank_monster("common_dropper", 1);
        common.drops = vec![MonsterDrop { code: "ore".into(), rate: 2, min_quantity: 1, max_quantity: 1 }]; // 1/2 chance
        let mut rare = blank_monster("rare_dropper", 1);
        rare.drops = vec![MonsterDrop { code: "ore".into(), rate: 100, min_quantity: 1, max_quantity: 1 }]; // 1/100 chance

        let data = blank_data(vec![rare, common]);
        let character = blank_character();
        let materials: HashSet<String> = ["ore".to_string()].into_iter().collect();

        let target = find_best_material_drop_target(&character, &data, &materials);
        assert_eq!(target.map(|t| t.code), Some("common_dropper".to_string()), "lower rate = more common = better");
    }

    #[test]
    fn find_best_material_drop_target_ignores_unrelated_drops() {
        let mut monster = blank_monster("irrelevant_dropper", 1);
        monster.drops = vec![MonsterDrop { code: "junk".into(), rate: 1, min_quantity: 1, max_quantity: 1 }];

        let data = blank_data(vec![monster]);
        let character = blank_character();
        let materials: HashSet<String> = ["ore".to_string()].into_iter().collect();

        assert!(find_best_material_drop_target(&character, &data, &materials).is_none());
    }

    #[test]
    fn find_best_material_drop_target_none_when_material_set_is_empty() {
        let mut monster = blank_monster("dropper", 1);
        monster.drops = vec![MonsterDrop { code: "ore".into(), rate: 1, min_quantity: 1, max_quantity: 1 }];

        let data = blank_data(vec![monster]);
        let character = blank_character();

        assert!(find_best_material_drop_target(&character, &data, &HashSet::new()).is_none());
    }
}
