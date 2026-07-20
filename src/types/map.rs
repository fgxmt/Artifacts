use serde::{Deserialize, Serialize};

use super::character::Character;
use super::envelope::Cooldown;

#[derive(Deserialize)]
pub struct Destination {
    pub x: i32,
    pub y: i32,
    pub name: String,
}

#[derive(Deserialize)]
pub struct MoveResponseData {
    pub destination: Destination,
    pub cooldown: Cooldown,
    pub character: Character,
}

pub struct MoveResult {
    pub cooldown: Cooldown,
    pub character: Character,
}

#[derive(Serialize)]
pub struct MoveBody {
    pub x: i32,
    pub y: i32,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct MapCondition {
    pub code: String,
    pub operator: String,
    pub value: i32,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct MapAccess {
    #[serde(rename = "type")]
    pub access_type: String,
    #[serde(default)]
    pub conditions: Vec<MapCondition>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct MapContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub code: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct MapTransition {
    pub map_id: i32,
    pub x: i32,
    pub y: i32,
    pub layer: String,
    #[serde(default)]
    pub conditions: Vec<MapCondition>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct MapInteractions {
    pub content: Option<MapContent>,
    pub transition: Option<MapTransition>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Map {
    pub map_id: i32,
    pub name: String,
    pub skin: String,
    pub x: i32,
    pub y: i32,
    pub layer: String,
    pub access: Option<MapAccess>,
    pub interactions: Option<MapInteractions>,
}
