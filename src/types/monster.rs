use serde::{Deserialize, Serialize};

use super::character::Character;
use super::envelope::Cooldown;

#[derive(Deserialize, Clone)]
pub struct Drop {
    pub quantity: u32,
    pub code: String,
}

#[derive(Deserialize, Clone)]
pub struct FightCharacter {
    pub character_name: String,
    pub xp: i32,
    pub gold: i32,
    pub drops: Option<Vec<Drop>>,
    pub final_hp: i32,
}

#[derive(Deserialize)]
pub struct Fight {
    pub result: String,
    pub turns: i32,
    pub opponent: String,
    pub logs: Vec<String>,
    pub characters: Vec<FightCharacter>,
}

#[derive(Deserialize)]
pub struct FightResponseData {
    pub cooldown: Cooldown,
    pub fight: Fight,
    pub characters: Vec<Character>,
}

pub struct FightResult {
    pub fight_stats: FightCharacter,
    pub cooldown: Cooldown,
    pub character: Character,
}

#[derive(Deserialize)]
pub struct RestResponseData {
    pub cooldown: Cooldown,
    pub character: Character,
}

pub struct RestResult {
    pub cooldown: Cooldown,
    pub character: Character,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct MonsterEffect {
    pub code: String,
    pub value: i32,
    pub description: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct MonsterDrop {
    pub code: String,
    pub rate: i32,
    pub min_quantity: i32,
    pub max_quantity: i32,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Monster {
    pub name: String,
    pub code: String,
    pub level: i32,
    #[serde(rename = "type")]
    pub monster_type: String,
    pub hp: i32,
    pub attack_fire: i32,
    pub attack_earth: i32,
    pub attack_water: i32,
    pub attack_air: i32,
    pub res_fire: i32,
    pub res_earth: i32,
    pub res_water: i32,
    pub res_air: i32,
    pub critical_strike: i32,
    pub initiative: i32,
    pub effects: Vec<MonsterEffect>,
    pub min_gold: i32,
    pub max_gold: i32,
    pub drops: Vec<MonsterDrop>,
}
