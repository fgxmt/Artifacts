use reqwest::Client;
use std::future::Future;
use std::time::Duration;

use crate::types::{Item, Map, Monster, Resource, Result};

use super::getters::{get_items, get_maps, get_monsters, get_resources};

pub const EQUIPMENT_TYPES: &[&str] = &[
    "weapon", "shield", "helmet", "body_armor", "leg_armor", "boots",
    "ring", "amulet", "artifact", "utility", "rune", "bag",
];

/// Static game data loaded once at startup and shared for the lifetime of the program.
#[derive(Clone)]
pub struct GameData {
    pub monsters: Vec<Monster>,
    pub items: Vec<Item>,
    pub resources: Vec<Resource>,
    pub maps: Vec<Map>,
    pub craftable_equip: Vec<Item>,
}

const INIT_RETRY_ATTEMPTS: u32 = 5;
const INIT_RETRY_DELAY: Duration = Duration::from_secs(2);

async fn retry<T, F, Fut>(label: &str, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut last_err = None;

    for attempt in 1..=INIT_RETRY_ATTEMPTS {
        match f().await {
            Ok(value) => return Ok(value),
            Err(e) => {
                eprintln!(
                    "[init] {} failed (attempt {}/{}): {}",
                    label, attempt, INIT_RETRY_ATTEMPTS, e
                );
                last_err = Some(e);
                if attempt < INIT_RETRY_ATTEMPTS {
                    tokio::time::sleep(INIT_RETRY_DELAY).await;
                }
            }
        }
    }

    Err(last_err.unwrap())
}

/// Loads and caches all reference game data (monsters, items, resources, maps,
/// and craftable equipment), retrying each getter on failure.
pub async fn load_game_data(client: &Client) -> Result<GameData> {
    let monsters  = retry("get_monsters",  || get_monsters(client)).await?;
    let items     = retry("get_items",     || get_items(client, None)).await?;
    let resources = retry("get_resources", || get_resources(client)).await?;
    let maps      = retry("get_maps",      || get_maps(client)).await?;

    let craftable_equip: Vec<Item> = items
        .iter()
        .filter(|i| i.craft.is_some() && EQUIPMENT_TYPES.contains(&i.item_type.as_str()))
        .cloned()
        .collect();

    Ok(GameData { monsters, items, resources, maps, craftable_equip })
}
