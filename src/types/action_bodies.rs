use serde::{Deserialize, Serialize};

use super::character::Character;
use super::envelope::Cooldown;
use super::item::Item;
use super::resource::{GatherDetails, GatherDrop};

#[derive(Serialize)]
pub struct DepositItem {
    pub code: String,
    pub quantity: i32,
}

#[derive(Deserialize)]
pub struct DepositResponseData {
    pub cooldown: Cooldown,
    pub character: Character,
}

pub struct DepositResult {
    pub cooldown: Cooldown,
    pub character: Character,
}

#[derive(Serialize)]
pub struct EquipBody {
    pub code: String,
    pub slot: String,
    pub quantity: i32,
}

#[derive(Serialize)]
pub struct UnequipBody {
    pub slot: String,
    pub quantity: i32,
}

#[derive(Deserialize)]
pub struct EquipSlotInfo {
    pub slot: String,
    pub item: Item,
}

#[derive(Deserialize)]
pub struct EquipResponseData {
    pub cooldown: Cooldown,
    pub items: Vec<EquipSlotInfo>,
    pub character: Character,
}

pub struct EquipResult {
    pub cooldown: Cooldown,
    pub character: Character,
}

#[derive(Serialize)]
pub struct CraftBody {
    pub code: String,
    pub quantity: i32,
}

#[derive(Deserialize)]
pub struct CraftResponseData {
    pub cooldown: Cooldown,
    pub details: GatherDetails,
    pub character: Character,
}

pub struct CraftResult {
    pub xp: i32,
    pub items: Vec<GatherDrop>,
    pub cooldown: Cooldown,
    pub character: Character,
}
