use reqwest::Client;

use crate::api::{move_character, wait_for_cooldown};
use crate::types::{Character, Result};

fn nearest_location<'a>(
    char_layer: &str,
    char_x: i32,
    char_y: i32,
    locs: &'a [(String, i32, i32)],
) -> Option<&'a (String, i32, i32)> {
    if locs.is_empty() { return None; }

    let same_layer: Vec<&(String, i32, i32)> = locs.iter()
        .filter(|(layer, _, _)| layer == char_layer)
        .collect();

    let candidates = if same_layer.is_empty() { locs.iter().collect::<Vec<_>>() } else { same_layer };

    candidates.into_iter().min_by_key(|(_, x, y)| (x - char_x).abs() + (y - char_y).abs())
}

/// Moves toward the nearest of `locs`, updating `character` in place on a successful move so
/// callers always see the character's real position afterward — critical for any caller that
/// keeps acting on `character` after this returns (e.g. `handle_flags` deciding whether it still
/// needs to navigate back to the bank for a later flag in the same batch). Propagates an error
/// once the underlying move action has exhausted its retries — the caller should treat that as
/// fatal for this cycle and restart.
pub(crate) async fn move_to_nearest(
    client: &Client,
    name: &'static str,
    character: &mut Character,
    locs: &[(String, i32, i32)],
    target_label: &str,
) -> Result<()> {
    if locs.iter().any(|(l, x, y)| l == &character.layer && *x == character.x && *y == character.y) {
        return Ok(());
    }

    match nearest_location(&character.layer, character.x, character.y, locs) {
        Some((layer, x, y)) if layer == &character.layer => {
            let result = move_character(client, name, *x, *y).await?;
            wait_for_cooldown(&result.cooldown).await;
            *character = result.character;
        }
        Some((layer, _, _)) => {
            eprintln!(
                "[{}] {} is on layer '{}' (character on '{}'); skipping movement",
                crate::ts_char(name), target_label, layer, character.layer
            );
        }
        None => {
            eprintln!("[{}] No map locations found for {}", crate::ts_char(name), target_label);
        }
    }

    Ok(())
}
