mod alchemy;
mod bank_ops;
mod consumables;
mod crafting_exec;
mod crafting_plan;
mod fight;
mod flag_handling;
mod gather;
mod grand_exchange;
mod init;
mod merchant;
mod movement;
mod promotion;
mod refining;
mod repositioning;
mod utility;

pub use alchemy::alchemy_and_crafting_loop;
pub use crafting_plan::print_initial_crafting_plan;
pub use fight::fight_loop;
pub use gather::gather_loop;
pub use grand_exchange::{plan_ge_purchases, print_ge_purchase_plan, GEPurchasePlan};
pub use init::init_character_ratings;

/// Seeds `GameState`'s cross-character promotion/targeting reference points — the dedicated
/// fighter's combat level and the alchemy-crafting character's min gear level/wishlist — from
/// their freshly-fetched `Character`s during program init, before any loop starts. Without this,
/// every promotion/drop-farming threshold would compare against the default level 1 until each
/// reference character's own loop happened to refresh it, which for the crafter's min gear level
/// specifically would make every fighting character wrongly think it should drop-farm from the
/// very first fight (level 1 is trivial to exceed).
pub fn seed_shared_state(state: &crate::flags::GameState, fighter: &crate::types::Character, crafter: &crate::types::Character) {
    state.set_fighter_combat_level(fighter.level);
    crafting_plan::refresh_shared_crafting_state(state, crafter);
}

/// What a character's loop is optimizing for — determines which optimizer to
/// re-run after `handle_flags` processes an `EquipUpgrade` flag.
#[derive(Clone, Copy)]
pub(crate) enum CharacterRole {
    Combat,
    Gathering(&'static str),
}

/// Pause before restarting a loop body after an action exhausted all its retries — avoids
/// hammering the API if the underlying issue (e.g. network outage) is still ongoing.
pub(crate) const RESTART_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) fn on_combat_level_up(name: &str, level: i32) {
    println!("[{}] *** Combat level up! Now level {} ***", crate::ts_char(name), level);
}

pub(crate) fn on_skill_level_up(name: &str, skill: &str, level: i32) {
    println!("[{}] *** {} level up! Now level {} ***", crate::ts_char(name), skill, level);
}
