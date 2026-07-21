//! Shared mechanics for characters that switch between their primary loop (gathering, or
//! alchemy-gathering-and-crafting) and fighting, based on how their own skill level compares to
//! the dedicated fighting character's combat level.
//!
//! Each promotable character's top-level `pub async fn X_loop` is a small dispatcher: it tracks
//! which mode it's currently in and calls either its primary-loop body or
//! [`super::fight::run_fight_mode`] (with `revert_skill: Some(skill)`). Both of those return
//! `Result<()>` where `Ok(())` specifically means "a mode-switch condition just became true,
//! please call the other one next" (neither body would otherwise ever return `Ok` — they only
//! return on an unrecoverable action failure, `Err`, which the dispatcher logs and retries in the
//! *same* mode instead of switching). This is exactly the pattern `run_fight_mode`'s own doc
//! comment describes for `revert_skill`; `should_promote` below is its mirror image for entering
//! fighting mode in the first place.

use crate::formulas::gather_promotion_threshold;
use crate::optimize::skill_level;
use crate::types::Character;

/// Whether `character`'s level in `skill` has reached the threshold to switch into fighting,
/// given the dedicated fighter's current combat level (`GameState::fighter_combat_level`) — the
/// same threshold `super::fight::run_fight_mode` checks in reverse to decide when to switch back.
/// Promotion can only become newly true from `character`'s own level increasing (the threshold is
/// monotonically non-decreasing in the fighter's level, so the fighter leveling up only ever makes
/// promotion *harder*), so callers only need to check this right after a level-up, not on every
/// action.
pub(crate) fn should_promote(character: &Character, skill: &str, fighter_combat_level: i32) -> bool {
    skill_level(character, skill) >= gather_promotion_threshold(fighter_combat_level)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_character() -> Character {
        Character {
            name: "test".into(), account: "test".into(), skin: "".into(),
            level: 1, xp: 0, max_xp: 0, gold: 0, speed: 0,
            mining_level: 1, mining_xp: 0, mining_max_xp: 0,
            woodcutting_level: 1, woodcutting_xp: 0, woodcutting_max_xp: 0,
            fishing_level: 1, fishing_xp: 0, fishing_max_xp: 0,
            weaponcrafting_level: 1, weaponcrafting_xp: 0, weaponcrafting_max_xp: 0,
            gearcrafting_level: 1, gearcrafting_xp: 0, gearcrafting_max_xp: 0,
            jewelrycrafting_level: 1, jewelrycrafting_xp: 0, jewelrycrafting_max_xp: 0,
            cooking_level: 1, cooking_xp: 0, cooking_max_xp: 0,
            alchemy_level: 1, alchemy_xp: 0, alchemy_max_xp: 0,
            hp: 100, max_hp: 100, haste: 0, critical_strike: 0, wisdom: 0,
            prospecting: 0, initiative: 0, threat: 0,
            attack_fire: 0, attack_earth: 0, attack_water: 0, attack_air: 0,
            dmg: 0, dmg_fire: 0, dmg_earth: 0, dmg_water: 0, dmg_air: 0,
            res_fire: 0, res_earth: 0, res_water: 0, res_air: 0,
            effects: vec![], x: 0, y: 0, layer: "interior".into(), map_id: 0,
            cooldown: 0, cooldown_expiration: None,
            weapon_slot: "".into(), rune_slot: "".into(), shield_slot: "".into(),
            helmet_slot: "".into(), body_armor_slot: "".into(), leg_armor_slot: "".into(),
            boots_slot: "".into(), ring1_slot: "".into(), ring2_slot: "".into(),
            amulet_slot: "".into(), artifact1_slot: "".into(), artifact2_slot: "".into(),
            artifact3_slot: "".into(), utility1_slot: "".into(), utility1_slot_quantity: 0,
            utility2_slot: "".into(), utility2_slot_quantity: 0, bag_slot: "".into(),
            task: "".into(), task_type: "".into(), task_progress: 0, task_total: 0,
            inventory_max_items: 100, inventory: vec![],
        }
    }

    #[test]
    fn should_promote_matches_gather_promotion_threshold() {
        let mut character = blank_character();

        // Fighter at combat level 13 -> threshold 20 (worked example from the tiers module).
        character.mining_level = 19;
        assert!(!should_promote(&character, "mining", 13), "19 is below the threshold of 20");
        character.mining_level = 20;
        assert!(should_promote(&character, "mining", 13), "20 meets the threshold of 20");
    }

    #[test]
    fn should_promote_is_skill_specific() {
        let mut character = blank_character();
        character.fishing_level = 20;
        character.cooking_level = 1; // deliberately left behind — must be ignored

        assert!(should_promote(&character, "fishing", 13));
    }
}
