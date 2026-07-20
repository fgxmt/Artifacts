use serde::{Deserialize, Serialize};

use super::character::Character;
use super::envelope::Cooldown;
use super::monster::MonsterDrop;

#[derive(Deserialize, Serialize, Clone)]
pub struct Resource {
    pub name: String,
    pub code: String,
    pub skill: String,
    pub level: i32,
    pub drops: Vec<MonsterDrop>,
}

#[derive(Deserialize, Clone)]
pub struct GatherDrop {
    pub code: String,
    pub quantity: i32,
}

#[derive(Deserialize)]
pub struct GatherDetails {
    pub xp: i32,
    pub items: Vec<GatherDrop>,
}

#[derive(Deserialize)]
pub struct GatherResponseData {
    pub cooldown: Cooldown,
    pub details: GatherDetails,
    pub character: Character,
}

pub struct GatherResult {
    pub xp: i32,
    pub items: Vec<GatherDrop>,
    pub cooldown: Cooldown,
    pub character: Character,
}
