use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct GEOrder {
    pub id: String,
    #[serde(rename = "type")]
    pub order_type: String,
    pub account: String,
    pub code: String,
    pub quantity: i32,
    pub price: i32,
    pub created_at: DateTime<Utc>,
}
