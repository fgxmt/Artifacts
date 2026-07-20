use reqwest::Client;

use crate::types::{
    ApiResponse, CraftBody, CraftResponseData, CraftResult, EquipBody, EquipResponseData,
    EquipResult, FightResponseData, FightResult, GatherResponseData, GatherResult, MoveBody,
    MoveResponseData, MoveResult, Result, RestResponseData, RestResult, UnequipBody,
};

use super::client::{send_with_retry, BASE_URL};

pub async fn fight_monster(client: &Client, name: &str) -> Result<FightResult> {
    let url = format!("{}/my/{}/action/fight", BASE_URL, name);
    let resp: ApiResponse<FightResponseData> = send_with_retry(|| client.post(&url)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let fight_data = resp.data.ok_or("no data in response")?;
    let cooldown   = fight_data.cooldown;
    let fight      = fight_data.fight;
    let character  = fight_data.characters.into_iter().next().ok_or("no character state in fight response")?;
    let fight_stats = fight.characters.into_iter().next().ok_or("no character in fight")?;

    println!("[{}] {}", crate::ts_char(name), if fight.result == "win" { "Fight won!" } else { "Fight lost!" });
    println!("[{}] XP gained: {} | HP remaining: {}", crate::ts_char(name), fight_stats.xp, fight_stats.final_hp);
    println!("[{}] Cooldown started: {} seconds", crate::ts_char(name), cooldown.total_seconds);

    if let Some(drops) = &fight_stats.drops {
        if !drops.is_empty() {
            let drops_str: Vec<String> = drops
                .iter()
                .map(|d| format!("{}x {}", d.quantity, d.code))
                .collect();
            println!("[{}] Loot dropped: {}", crate::ts_char(name), drops_str.join(", "));
        }
    }

    Ok(FightResult { fight_stats, cooldown, character })
}

pub async fn rest_character(client: &Client, name: &str) -> Result<RestResult> {
    let url = format!("{}/my/{}/action/rest", BASE_URL, name);
    let resp: ApiResponse<RestResponseData> = send_with_retry(|| client.post(&url)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    println!("[{}] Resting... Cooldown started: {} seconds", crate::ts_char(name), data.cooldown.total_seconds);

    Ok(RestResult { cooldown: data.cooldown, character: data.character })
}

pub async fn move_character(
    client: &Client,
    name: &str,
    x: i32,
    y: i32,
) -> Result<MoveResult> {
    let url = format!("{}/my/{}/action/move", BASE_URL, name);
    let body = MoveBody { x, y };
    let resp: ApiResponse<MoveResponseData> = send_with_retry(|| client.post(&url).json(&body)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    println!(
        "[{}] Moved to ({}, {}) on {}",
        crate::ts_char(name), data.destination.x, data.destination.y, data.destination.name
    );
    println!("[{}] Cooldown started: {} seconds", crate::ts_char(name), data.cooldown.total_seconds);

    Ok(MoveResult { cooldown: data.cooldown, character: data.character })
}

/// Equips every item in `items` in a single API call — the endpoint natively accepts a batch, so
/// swapping several slots at once (e.g. several equip-upgrades found in the bank at the same
/// time) costs one action/cooldown instead of one per item.
pub async fn equip_items(
    client: &Client,
    name: &str,
    items: Vec<EquipBody>,
) -> Result<EquipResult> {
    let url = format!("{}/my/{}/action/equip", BASE_URL, name);
    let resp: ApiResponse<EquipResponseData> = send_with_retry(|| client.post(&url).json(&items)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    let summary: Vec<String> = data.items.iter().map(|i| format!("{} in slot {}", i.item.code, i.slot)).collect();
    println!("[{}] Equipped: {}", crate::ts_char(name), summary.join(", "));
    println!("[{}] Cooldown started: {} seconds", crate::ts_char(name), data.cooldown.total_seconds);

    Ok(EquipResult { cooldown: data.cooldown, character: data.character })
}

/// Unequips every slot in `items` in a single API call — see `equip_items`.
pub async fn unequip_items(
    client: &Client,
    name: &str,
    items: Vec<UnequipBody>,
) -> Result<EquipResult> {
    let url = format!("{}/my/{}/action/unequip", BASE_URL, name);
    let slots: Vec<String> = items.iter().map(|i| i.slot.clone()).collect();
    let resp: ApiResponse<EquipResponseData> = send_with_retry(|| client.post(&url).json(&items)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    println!("[{}] Unequipped slot(s): {}", crate::ts_char(name), slots.join(", "));
    println!("[{}] Cooldown started: {} seconds", crate::ts_char(name), data.cooldown.total_seconds);

    Ok(EquipResult { cooldown: data.cooldown, character: data.character })
}

pub async fn craft_item(
    client: &Client,
    name: &str,
    code: &str,
    quantity: i32,
) -> Result<CraftResult> {
    let url = format!("{}/my/{}/action/crafting", BASE_URL, name);
    let body = CraftBody { code: code.to_string(), quantity };
    let resp: ApiResponse<CraftResponseData> = send_with_retry(|| client.post(&url).json(&body)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    println!("[{}] Crafted {}x {} | XP gained: {}", crate::ts_char(name), quantity, code, data.details.xp);

    if !data.details.items.is_empty() {
        let items_str: Vec<String> = data.details.items
            .iter()
            .map(|i| format!("{}x {}", i.quantity, i.code))
            .collect();
        println!("[{}] Items: {}", crate::ts_char(name), items_str.join(", "));
    }

    println!("[{}] Cooldown started: {} seconds", crate::ts_char(name), data.cooldown.total_seconds);

    Ok(CraftResult {
        xp: data.details.xp,
        items: data.details.items,
        cooldown: data.cooldown,
        character: data.character,
    })
}

pub async fn use_transition(client: &Client, name: &str) -> Result<MoveResult> {
    let url = format!("{}/my/{}/action/transition", BASE_URL, name);
    let resp: ApiResponse<MoveResponseData> = send_with_retry(|| client.post(&url)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    println!(
        "[{}] Transitioned to ({}, {}) on {}",
        crate::ts_char(name), data.destination.x, data.destination.y, data.destination.name
    );
    println!("[{}] Cooldown started: {} seconds", crate::ts_char(name), data.cooldown.total_seconds);

    Ok(MoveResult { cooldown: data.cooldown, character: data.character })
}

pub async fn gather_material(client: &Client, name: &str) -> Result<GatherResult> {
    let url = format!("{}/my/{}/action/gathering", BASE_URL, name);
    let resp: ApiResponse<GatherResponseData> = send_with_retry(|| client.post(&url)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    println!("[{}] Gathered! XP gained: {}", crate::ts_char(name), data.details.xp);

    if !data.details.items.is_empty() {
        let items_str: Vec<String> = data.details.items
            .iter()
            .map(|i| format!("{}x {}", i.quantity, i.code))
            .collect();
        println!("[{}] Items: {}", crate::ts_char(name), items_str.join(", "));
    }

    println!("[{}] Cooldown started: {} seconds", crate::ts_char(name), data.cooldown.total_seconds);

    Ok(GatherResult {
        xp: data.details.xp,
        items: data.details.items,
        cooldown: data.cooldown,
        character: data.character,
    })
}
