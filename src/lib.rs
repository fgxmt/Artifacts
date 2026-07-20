pub mod secrets;
pub mod types;
pub mod api;
pub mod flags;
pub mod formulas;
pub mod loops;
pub mod optimize;

pub use api::{
    build_client, craft_item, deposit_items, deposit_to_bank, equip_items, fight_monster,
    gather_material, get_bank_items, get_character, get_items, get_maps, get_monsters,
    get_resources, is_inventory_full, load_game_data, move_character, rest_character,
    unequip_items, use_transition, wait_for_cooldown, withdraw_items, GameData,
};
pub use formulas::{
    calculate_cooldown, calculate_crafting_xp, calculate_gathering_cooldown,
    calculate_gathering_xp, calculate_xp, craft_xp_per_ingredient, craftable_quantity,
    is_recyclable_for_wishlist, recycle_return_quantity, recycle_returns_per_ingredient,
    xp_to_next_level,
};
pub use optimize::{find_optimal_crafting, find_optimal_gathering, find_optimal_monster, GatherTarget, MonsterTarget};
pub use types::{
    BankItem, Character, Cooldown, CraftBody, CraftInfo, CraftIngredient, CraftResult, DepositItem,
    DepositResult, Destination, Drop, Effect, EquipResult, Error, FightCharacter, FightResult,
    GatherDrop, GatherResult, InventorySlot, Item, ItemCondition, ItemEffect, Map, MapAccess,
    MapCondition, MapContent, MapInteractions, MapTransition, Monster, MonsterDrop, MonsterEffect,
    MoveResult, Resource, RestResult, Result,
};

pub fn ts() -> String {
    let now = chrono::Local::now();
    format!("{}.{:02}", now.format("%d/%m/%Y %H:%M:%S"), now.timestamp_subsec_millis() / 10)
}

pub fn ts_char(name: &str) -> String {
    let now = chrono::Local::now();
    format!("{}.{:02} | {}", now.format("%d/%m/%Y %H:%M:%S"), now.timestamp_subsec_millis() / 10, name)
}
