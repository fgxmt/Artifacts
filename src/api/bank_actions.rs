use reqwest::Client;
use std::time::Duration;

use crate::flags::GameState;
use crate::types::{
    ApiResponse, BankDetails, BankItem, Character, DepositItem, DepositResponseData,
    DepositResult, GoldBody, PagedResponse, Result,
};

use super::character_actions::move_character;
use super::client::{decode, send_with_retry, wait_for_cooldown, BASE_URL};

pub async fn deposit_items(
    client: &Client,
    name: &str,
    items: Vec<DepositItem>,
) -> Result<DepositResult> {
    let url = format!("{}/my/{}/action/bank/deposit/item", BASE_URL, name);
    let resp: ApiResponse<DepositResponseData> = send_with_retry(|| client.post(&url).json(&items)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    Ok(DepositResult { cooldown: data.cooldown, character: data.character })
}

pub async fn withdraw_items(
    client: &Client,
    name: &str,
    items: Vec<DepositItem>,
) -> Result<DepositResult> {
    let url = format!("{}/my/{}/action/bank/withdraw/item", BASE_URL, name);
    let count = items.len();
    let requested: Vec<String> = items.iter().map(|i| format!("{}x {}", i.quantity, i.code)).collect();
    println!("[{}] Requesting bank withdrawal: {}", crate::ts_char(name), requested.join(", "));

    let resp: ApiResponse<DepositResponseData> = send_with_retry(|| client.post(&url).json(&items)).await?;

    if let Some(err) = resp.error {
        eprintln!(
            "[{}] Bank withdrawal FAILED ({}): requested {}",
            crate::ts_char(name), err.message, requested.join(", ")
        );
        // The planner sizes requests against a cached bank snapshot, which can go stale between
        // planning and this request actually landing (another character can deposit/withdraw the
        // same item in between) — pull a live snapshot so the log shows exactly which item(s), if
        // any, the bank genuinely came up short on right now, versus a bug in the request itself
        // (e.g. requesting an item the bank never had, or a duplicate/zero-quantity entry).
        match get_bank_items(client).await {
            Ok(bank) => {
                for item in &items {
                    let actual = bank.iter().find(|b| b.code == item.code).map(|b| b.quantity).unwrap_or(0);
                    let flag = if actual < item.quantity { "  <-- SHORT" } else { "" };
                    eprintln!(
                        "[{}]   {}: requested {}, bank currently has {}{}",
                        crate::ts_char(name), item.code, item.quantity, actual, flag
                    );
                }
            }
            Err(e) => eprintln!("[{}] Failed to fetch live bank contents for diagnostics: {}", crate::ts_char(name), e),
        }
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    println!("[{}] Withdrew {} item stack(s) from bank", crate::ts_char(name), count);
    println!("[{}] Cooldown started: {} seconds", crate::ts_char(name), data.cooldown.total_seconds);

    Ok(DepositResult { cooldown: data.cooldown, character: data.character })
}

pub async fn deposit_gold(client: &Client, name: &str, quantity: i32) -> Result<DepositResult> {
    let url = format!("{}/my/{}/action/bank/deposit/gold", BASE_URL, name);
    let body = GoldBody { quantity };
    let resp: ApiResponse<DepositResponseData> = send_with_retry(|| client.post(&url).json(&body)).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    let data = resp.data.ok_or("no data in response")?;
    Ok(DepositResult { cooldown: data.cooldown, character: data.character })
}

/// Moves to bank, deposits all inventory items, refreshes bank state, then returns to origin.
/// Each individual action already retries internally; an `Err` here means every retry for some
/// step was exhausted, and the caller should treat this cycle as failed and restart.
pub async fn deposit_to_bank(
    client: &Client,
    name: &str,
    character: &Character,
    state: &GameState,
) -> Result<Character> {
    let origin_x = character.x;
    let origin_y = character.y;

    let result = move_character(client, name, 4, 1).await?;
    wait_for_cooldown(&result.cooldown).await;

    let items: Vec<DepositItem> = character
        .inventory
        .iter()
        .filter(|s| s.quantity > 0 && !s.code.is_empty())
        .map(|s| DepositItem { code: s.code.clone(), quantity: s.quantity })
        .collect();

    if !items.is_empty() {
        println!("[{}] Depositing {} item stack(s) to bank...", crate::ts_char(name), items.len());
        let result = deposit_items(client, name, items).await?;
        wait_for_cooldown(&result.cooldown).await;

        let bank = get_bank_items(client).await?;
        let total: i32 = bank.iter().map(|b| b.quantity).sum();
        println!("[{}] Bank updated: {} stacks ({} total items)", crate::ts_char(name), bank.len(), total);
        state.update_bank(bank).await;
    }

    if character.gold > 0 {
        println!("[{}] Depositing {} gold to bank...", crate::ts_char(name), character.gold);
        let result = deposit_gold(client, name, character.gold).await?;
        wait_for_cooldown(&result.cooldown).await;
    }

    let result = move_character(client, name, origin_x, origin_y).await?;
    wait_for_cooldown(&result.cooldown).await;
    Ok(result.character)
}

/// This endpoint is capped at 100 items per page (unlike the other info getters), so it needs
/// real pagination — 1s between page requests to stay well clear of the rate limit.
pub async fn get_bank_items(client: &Client) -> Result<Vec<BankItem>> {
    let mut all_items = Vec::new();
    let mut page = 1i32;

    loop {
        let url = format!("{}/my/bank/items?page={}&size=100", BASE_URL, page);
        let resp: PagedResponse<BankItem> = decode(client.get(&url).send().await?).await?;

        if let Some(err) = resp.error {
            return Err(err.message.into());
        }

        all_items.extend(resp.data.unwrap_or_default());

        let total_pages = resp.pages.unwrap_or(1);
        if page >= total_pages { break; }
        page += 1;

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(all_items)
}

/// Bank account details — slots, expansions, and (unlike anything on `Character`) the bank's own
/// shared gold balance, separate from any character's currently-held gold.
pub async fn get_bank_details(client: &Client) -> Result<BankDetails> {
    let url = format!("{}/my/bank", BASE_URL);
    let resp: ApiResponse<BankDetails> = decode(client.get(&url).send().await?).await?;

    if let Some(err) = resp.error {
        return Err(err.message.into());
    }

    resp.data.ok_or_else(|| "no data in response".into())
}
