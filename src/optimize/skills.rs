use crate::types::Character;

pub fn skill_level(character: &Character, skill: &str) -> i32 {
    match skill {
        "fishing"         => character.fishing_level,
        "mining"          => character.mining_level,
        "woodcutting"     => character.woodcutting_level,
        "weaponcrafting"  => character.weaponcrafting_level,
        "gearcrafting"    => character.gearcrafting_level,
        "jewelrycrafting" => character.jewelrycrafting_level,
        "cooking"         => character.cooking_level,
        "alchemy"         => character.alchemy_level,
        _                 => 1,
    }
}

/// Maps a gathering skill to the skill its raw materials are refined with. Woodcutting/mining
/// refine into their own skill (planks/bars/gems); fishing "refines" via cooking.
pub fn refining_skill_for(gather_skill: &str) -> &'static str {
    match gather_skill {
        "woodcutting" => "woodcutting",
        "mining"      => "mining",
        "fishing"     => "cooking",
        _             => "",
    }
}

/// (level, xp, max_xp) for one of the crafting skills the next-tier wishlist tracks.
pub fn skill_xp_progress(character: &Character, skill: &str) -> (i32, i32, i32) {
    match skill {
        "weaponcrafting"  => (character.weaponcrafting_level, character.weaponcrafting_xp, character.weaponcrafting_max_xp),
        "gearcrafting"    => (character.gearcrafting_level, character.gearcrafting_xp, character.gearcrafting_max_xp),
        "jewelrycrafting" => (character.jewelrycrafting_level, character.jewelrycrafting_xp, character.jewelrycrafting_max_xp),
        "alchemy"         => (character.alchemy_level, character.alchemy_xp, character.alchemy_max_xp),
        _                 => (1, 0, 0),
    }
}
