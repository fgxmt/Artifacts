use reqwest::Client;

use crate::types::{ApiResponse, Character, Map, Monster, PagedResponse, Resource, Item, Result};

use super::client::{decode, BASE_URL};

pub fn is_inventory_full(character: &Character) -> bool {
    let occupied = character
        .inventory
        .iter()
        .filter(|s| s.quantity > 0 && !s.code.is_empty())
        .count();
    let total: i32 = character.inventory.iter().map(|s| s.quantity).sum();
    occupied >= 20 || total >= character.inventory_max_items
}

pub async fn get_character(client: &Client, name: &str) -> Result<Character> {
    let url = format!("{}/characters/{}", BASE_URL, name);
    let resp: ApiResponse<Character> = decode(client.get(&url).send().await?).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    resp.data.ok_or_else(|| "no data in response".into())
}

pub async fn get_items(client: &Client, craft_skill: Option<&str>) -> Result<Vec<Item>> {
    let url = match craft_skill {
        Some(skill) => format!("{}/items?size=10000&craft_skill={}", BASE_URL, skill),
        None        => format!("{}/items?size=10000", BASE_URL),
    };
    let resp: PagedResponse<Item> = decode(client.get(&url).send().await?).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    Ok(resp.data.unwrap_or_default())
}

pub async fn get_resources(client: &Client) -> Result<Vec<Resource>> {
    let url = format!("{}/resources?size=10000", BASE_URL);
    let resp: PagedResponse<Resource> = decode(client.get(&url).send().await?).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    Ok(resp.data.unwrap_or_default())
}

pub async fn get_maps(client: &Client) -> Result<Vec<Map>> {
    let url = format!("{}/maps?size=10000", BASE_URL);
    let resp: PagedResponse<Map> = decode(client.get(&url).send().await?).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    Ok(resp.data.unwrap_or_default())
}

pub async fn get_monsters(client: &Client) -> Result<Vec<Monster>> {
    let url = format!("{}/monsters?size=10000", BASE_URL);
    let resp: PagedResponse<Monster> = decode(client.get(&url).send().await?).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    Ok(resp.data.unwrap_or_default())
}
