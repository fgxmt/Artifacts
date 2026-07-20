mod alchemy;
mod bank_ops;
mod crafting_exec;
mod crafting_plan;
mod fight;
mod flag_handling;
mod gather;
mod init;
mod movement;
mod refining;
mod repositioning;

pub use alchemy::alchemy_and_crafting_loop;
pub use crafting_plan::print_initial_crafting_plan;
pub use fight::fight_loop;
pub use gather::gather_loop;
pub use init::init_character_ratings;

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
