use chrono::{DateTime, Utc};
use serde::Deserialize;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Deserialize)]
pub struct ApiError {
    pub message: String,
}

#[derive(Deserialize)]
pub struct ApiResponse<T> {
    pub data: Option<T>,
    pub error: Option<ApiError>,
}

#[derive(Deserialize)]
pub struct PagedResponse<T> {
    pub data: Option<Vec<T>>,
    pub total: Option<i32>,
    pub page: Option<i32>,
    pub size: Option<i32>,
    pub pages: Option<i32>,
    pub error: Option<ApiError>,
}

#[derive(Deserialize, Clone)]
pub struct Cooldown {
    pub total_seconds: f64,
    pub remaining_seconds: f64,
    pub started_at: DateTime<Utc>,
    pub expiration: DateTime<Utc>,
    pub reason: String,
}
