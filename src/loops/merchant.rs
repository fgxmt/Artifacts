//! Placeholder for the merchant-type character behavior — not yet defined (see `FlagAction::
//! MerchantSummon`'s doc comment). `run_merchant_stint` is what `handle_flags` calls whenever a
//! `MerchantSummon` flag is drained, in either the fishing character's fishing or promoted-
//! fighting mode; for now it's a no-op that just returns control to whichever mode triggered it.

use crate::types::{Character, Result};

pub(crate) async fn run_merchant_stint(name: &'static str, character: Character) -> Result<Character> {
    println!(
        "[{}] Merchant flag received — merchant behavior isn't implemented yet, resuming previous activity.",
        crate::ts_char(name)
    );
    Ok(character)
}
