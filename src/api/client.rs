use reqwest::{
    header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE},
    Client, RequestBuilder,
};
use serde::de::DeserializeOwned;
use std::time::Duration;

use chrono::Utc;

use crate::secrets::TOKEN;
use crate::types::{Cooldown, Result};

pub(crate) const BASE_URL: &str = "https://api.artifactsmmo.com";

/// Decodes an API response body as JSON. On failure, prints the full raw response body (so a
/// malformed/unexpected payload is visible for debugging) before returning a short error.
pub(crate) async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let text = response.text().await?;
    serde_json::from_str::<T>(&text).map_err(|e| {
        eprintln!("Error decoding response body: {}\nResponse body: {}", e, text);
        format!("JSON decode error: {}", e).into()
    })
}

// ── Retry / backoff ──────────────────────────────────────────────────────────

/// Base delays for each retry of a failed action call: ~0.5s, ~2s, ~8s, ~16s, ~1min.
const RETRY_DELAYS_MS: [u64; 5] = [500, 2_000, 8_000, 16_000, 60_000];

/// Applies ±10% pseudo-random jitter to `base_ms`, without pulling in a `rand` dependency.
fn jittered_delay(base_ms: u64) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let frac = (nanos as f64 / u32::MAX as f64) * 2.0 - 1.0; // in [-1.0, 1.0]
    let ms = (base_ms as f64 * (1.0 + frac * 0.10)).max(0.0) as u64;
    Duration::from_millis(ms)
}

/// Sends the request (re)built by `build`, decoding it as `T`. Retries on failure per
/// `RETRY_DELAYS_MS` (~0.5s/2s/8s/16s/1min, ±10% jitter) — 5 retries, 6 attempts total. Returns
/// the last error once every attempt has failed, for the caller to treat as fatal for this cycle.
pub(crate) async fn send_with_retry<T, F>(build: F) -> Result<T>
where
    T: DeserializeOwned,
    F: Fn() -> RequestBuilder,
{
    let total_attempts = RETRY_DELAYS_MS.len() + 1;
    let mut last_err: Option<crate::types::Error> = None;

    // Pair each attempt with the delay to use if it fails — every retry gets a delay from the
    // schedule; the final attempt gets None, since there's nothing left to retry into.
    let schedule = RETRY_DELAYS_MS.iter().copied().map(Some).chain(std::iter::once(None));

    for (attempt, delay_ms) in schedule.enumerate() {
        let outcome: Result<T> = async {
            let response = build().send().await?;
            decode(response).await
        }.await;

        match outcome {
            Ok(value) => return Ok(value),
            Err(e) => {
                if let Some(ms) = delay_ms {
                    let delay = jittered_delay(ms);
                    eprintln!(
                        "Action request failed (attempt {}/{}), retrying in {:.1}s: {}",
                        attempt + 1, total_attempts, delay.as_secs_f64(), e
                    );
                    tokio::time::sleep(delay).await;
                }
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap())
}

pub fn build_client() -> Client {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", TOKEN)).expect("valid token"),
    );
    Client::builder()
        .default_headers(headers)
        .build()
        .expect("failed to build HTTP client")
}

pub async fn wait_for_cooldown(cooldown: &Cooldown) {
    let now = Utc::now();
    if cooldown.expiration > now {
        let duration = (cooldown.expiration - now).to_std().unwrap_or(Duration::ZERO);
        tokio::time::sleep(duration).await;
    }
}
