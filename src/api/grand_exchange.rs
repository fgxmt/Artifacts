use std::time::Duration;

use reqwest::Client;

use crate::types::{GEOrder, PagedResponse, Result};

use super::client::{decode, BASE_URL};

/// Fetches every open sell order for `code` on the Grand Exchange, paginated (mirrors
/// `get_bank_items`'s loop). Read-only — querying the Grand Exchange doesn't require the
/// character to be at its location, unlike posting a buy/sell order there.
pub async fn get_ge_sell_orders(client: &Client, code: &str) -> Result<Vec<GEOrder>> {
    let mut all_orders = Vec::new();
    let mut page = 1i32;

    loop {
        let url = format!("{}/grandexchange/orders?code={}&type=sell&page={}&size=100", BASE_URL, code, page);
        let resp: PagedResponse<GEOrder> = decode(client.get(&url).send().await?).await?;

        if let Some(err) = resp.error {
            return Err(err.message.into());
        }

        all_orders.extend(resp.data.unwrap_or_default());

        let total_pages = resp.pages.unwrap_or(1);
        if page >= total_pages { break; }
        page += 1;

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(all_orders)
}
