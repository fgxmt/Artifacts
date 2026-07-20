use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone)]
pub struct ItemCondition {
    pub code: String,
    pub operator: String,
    pub value: i32,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ItemEffect {
    pub code: String,
    pub value: i32,
    pub description: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct CraftIngredient {
    pub code: String,
    pub quantity: i32,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct CraftInfo {
    pub skill: Option<String>,
    pub level: Option<i32>,
    pub items: Vec<CraftIngredient>,
    pub quantity: i32,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Item {
    pub name: String,
    pub code: String,
    pub level: i32,
    #[serde(rename = "type")]
    pub item_type: String,
    pub subtype: String,
    pub description: String,
    #[serde(default)]
    pub conditions: Vec<ItemCondition>,
    #[serde(default)]
    pub effects: Vec<ItemEffect>,
    pub craft: Option<CraftInfo>,
    pub tradeable: bool,
    pub recyclable: bool,
}
