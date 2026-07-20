use reqwest::Client;

use crate::api::{get_character, GameData};
use crate::formulas::craft_xp_per_ingredient;
use crate::types::{Item, Map, MonsterDrop, Resource};

use super::combat_sim::{evaluate_average, minimum_hp_threshold};
use super::gathering_sim::gathering_xp_per_hour;
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
            avg_xp_per_hour:  evaluate_average(&character, m),
            min_hp_threshold: minimum_hp_threshold(&character, m),
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
