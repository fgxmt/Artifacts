use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::api::GameData;
use crate::optimize::{current_slot_rating, equipped_codes, is_healing_consumable, SlotRating};
use crate::types::{BankItem, Character};

// ── Flag action types ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BankItemRequest {
    pub code: String,
    pub quantity: i32,
}

#[derive(Debug, Clone)]
pub enum FlagAction {
    RetrieveFromBank(Vec<BankItemRequest>),
    /// Unequip whatever is in `slot` and equip `item_code` in its place — set
    /// when another character crafts an item that's a ratings upgrade for this one.
    EquipUpgrade { slot: String, item_code: String },
    /// The bank now holds at least one healing consumable again — set for a fight-loop character
    /// that previously found both its inventory and the bank empty of them (see
    /// `GameState::await_healing_consumables`), so it can stop polling and go restock the moment
    /// any character's bank trip changes that.
    HealingConsumablesAvailable,
    /// The bank now holds enough materials to craft at least one wishlisted equipment item
    /// (weaponcrafting/gearcrafting/jewelrycrafting, not alchemy) — set for the alchemy-crafting
    /// character, whether it's currently gathering or fighting, so it can go withdraw and craft it
    /// (see `GameState::set_wishlist` / `update_bank`).
    CraftWishlistGearAvailable,
    /// A merchant task has been requested — set for the fishing character, in either its fishing
    /// or promoted-fighting mode, to temporarily run the (placeholder) merchant loop before
    /// returning to whichever mode it was in.
    MerchantSummon,
    // More flag actions can be added here
}

#[derive(Debug, Clone)]
pub struct Flag {
    pub from_character: String,
    pub action: FlagAction,
}

// ── Shared game state ────────────────────────────────────────────────────────

pub struct GameState {
    flags: Mutex<HashMap<String, Vec<Flag>>>,
    bank:  Mutex<Vec<BankItem>>,
    /// Reference game data (monsters, items, resources, maps) loaded once at
    /// program startup; immutable for the lifetime of the process.
    pub data: GameData,
    /// Latest per-slot item ranking for each character, refreshed every time
    /// that character's loop re-runs `optimize_items`. Used by crafters to
    /// decide which items are upgrades for which characters/slots without an
    /// extra `get_character` round-trip. A plain std Mutex is enough since
    /// access is a quick read/replace with no `.await` held across the lock.
    item_ratings: std::sync::Mutex<HashMap<String, Vec<SlotRating>>>,
    /// Each character's currently-equipped item code per slot, refreshed alongside `item_ratings`
    /// every time `set_item_ratings` runs (callers always have a fresh `Character` in hand at that
    /// point). Used to avoid re-flagging a bank item a character has already equipped just because
    /// a spare copy is still sitting in the bank.
    equipped: std::sync::Mutex<HashMap<String, HashMap<String, String>>>,
    /// Direct ingredients (item code -> quantity) currently needed for recipes that can't be
    /// crafted yet (missing other ingredients or crafting-skill level) — refreshed by the crafting
    /// loop each planning pass (`loops::plan_crafting_priority`) from two sources, merged: (1)
    /// equip-upgrade recipes (`allocate_upgrade_crafts`) and (2) next-tier skill-XP wishlist
    /// recipes (`build_wishlist`/`classify_wishlist_craftable`). Equip upgrades get first claim on
    /// scarce shared materials — they're allocated before the wishlist runs, against the same
    /// remaining-supply pool — so this map's contents skew toward upgrades whenever the two
    /// compete. Gatherers read this to know which materials to hold back from refining/other use;
    /// it deliberately only ever holds an ingredient's "highest stage" (e.g. a bar, not the ore
    /// under it), since refining toward it should still proceed freely.
    reserved_materials: std::sync::Mutex<HashMap<String, i32>>,
    /// Name of the fight-loop character currently waiting to be told the bank has a healing
    /// consumable again (`None` most of the time) — see `await_healing_consumables` and
    /// `update_bank`. A push rather than a poll: without this, a character that found the bank
    /// empty would have no way to learn it's been restocked short of re-checking on every fight.
    awaiting_healing_consumables: std::sync::Mutex<Option<String>>,
    /// The dedicated fighting character's current combat level — seeded during program init and
    /// kept fresh by that character's own loop on every level-up. Read by mining/woodcutting/
    /// fishing/alchemy characters (via `formulas::gather_promotion_threshold`) to decide whether
    /// their own skill level has outpaced it enough to switch into fighting themselves (see
    /// `loops::promotion`). Defaults to 1 until seeded, so promotion logic must not run before
    /// program init has called `set_fighter_combat_level` at least once.
    fighter_combat_level: std::sync::Mutex<i32>,
    /// The alchemy-crafting character's lowest of weaponcrafting/gearcrafting/jewelrycrafting
    /// level — kept fresh the same way as `fighter_combat_level`. Read by every currently-fighting
    /// character (original or promoted) to decide whether to prioritize wishlist-material-drop
    /// farming over plain combat XP/hour (see `loops::repositioning`).
    crafter_min_gear_level: std::sync::Mutex<i32>,
    /// Name of the alchemy-crafting character that owns `wishlist_equipment_codes`/
    /// `wishlist_alchemy_codes` — `None` until it's set them at least once.
    wishlist_owner: std::sync::Mutex<Option<String>>,
    /// Current wishlist item codes for weaponcrafting/gearcrafting/jewelrycrafting, refreshed every
    /// cycle (in either of its modes) by the alchemy-crafting character. Used by `update_bank` to
    /// flag it the moment the bank holds enough materials to craft one, and by fighting characters
    /// to prioritize monsters that drop those materials (see `FlagAction::CraftWishlistGearAvailable`
    /// and `loops::repositioning`).
    wishlist_equipment_codes: std::sync::Mutex<Vec<String>>,
    /// Same as `wishlist_equipment_codes` but for alchemy — only consulted as a fallback for
    /// drop-farming when no equipment-wishlist material is droppable by anything guaranteed-
    /// beatable; never used for the bank-deposit craft trigger (that's equipment-only).
    wishlist_alchemy_codes: std::sync::Mutex<Vec<String>>,
}

impl GameState {
    pub fn new(data: GameData) -> Arc<Self> {
        Arc::new(Self {
            flags: Mutex::new(HashMap::new()),
            bank:  Mutex::new(Vec::new()),
            data,
            item_ratings: std::sync::Mutex::new(HashMap::new()),
            equipped: std::sync::Mutex::new(HashMap::new()),
            reserved_materials: std::sync::Mutex::new(HashMap::new()),
            awaiting_healing_consumables: std::sync::Mutex::new(None),
            fighter_combat_level: std::sync::Mutex::new(1),
            crafter_min_gear_level: std::sync::Mutex::new(1),
            wishlist_owner: std::sync::Mutex::new(None),
            wishlist_equipment_codes: std::sync::Mutex::new(Vec::new()),
            wishlist_alchemy_codes: std::sync::Mutex::new(Vec::new()),
        })
    }

    pub fn set_item_ratings(&self, name: &str, ratings: Vec<SlotRating>, character: &Character) {
        self.item_ratings.lock().unwrap().insert(name.to_string(), ratings);
        self.equipped.lock().unwrap().insert(name.to_string(), equipped_codes(character));
    }

    /// Records that `char_name` now has `item_code` equipped in `slot` — called right after a
    /// successful equip, so the very next bank-upgrade check doesn't re-flag a spare copy of the
    /// same item still sitting in the bank before `set_item_ratings` happens to run again (which,
    /// for a gathering character, might not be until their next combat-unrelated event never — it
    /// only runs on combat level-ups and program init).
    pub fn set_equipped_slot(&self, char_name: &str, slot: &str, item_code: &str) {
        self.equipped.lock().unwrap()
            .entry(char_name.to_string())
            .or_default()
            .insert(slot.to_string(), item_code.to_string());
    }

    pub fn all_item_ratings(&self) -> HashMap<String, Vec<SlotRating>> {
        self.item_ratings.lock().unwrap().clone()
    }

    pub fn equipped_snapshot(&self, char_name: &str) -> HashMap<String, String> {
        self.equipped.lock().unwrap().get(char_name).cloned().unwrap_or_default()
    }

    pub fn set_reserved_materials(&self, reserved: HashMap<String, i32>) {
        *self.reserved_materials.lock().unwrap() = reserved;
    }

    pub fn reserved_materials_snapshot(&self) -> HashMap<String, i32> {
        self.reserved_materials.lock().unwrap().clone()
    }

    /// Registers `name` as waiting to be told the bank has a healing consumable again — call this
    /// only after confirming both the character's inventory and the bank are currently empty of
    /// them, so there's nothing else to do but wait for `update_bank` to notice a restock.
    pub fn await_healing_consumables(&self, name: &str) {
        *self.awaiting_healing_consumables.lock().unwrap() = Some(name.to_string());
    }

    pub fn set_fighter_combat_level(&self, level: i32) {
        *self.fighter_combat_level.lock().unwrap() = level;
    }

    pub fn fighter_combat_level(&self) -> i32 {
        *self.fighter_combat_level.lock().unwrap()
    }

    pub fn set_crafter_min_gear_level(&self, level: i32) {
        *self.crafter_min_gear_level.lock().unwrap() = level;
    }

    pub fn crafter_min_gear_level(&self) -> i32 {
        *self.crafter_min_gear_level.lock().unwrap()
    }

    /// Refreshes the alchemy-crafting character's wishlist snapshot — `equipment_codes` are
    /// weaponcrafting/gearcrafting/jewelrycrafting wishlist item codes (drives the bank-deposit
    /// craft trigger and fighting characters' primary drop-farming target), `alchemy_codes` are
    /// alchemy wishlist item codes (drop-farming fallback only, see `wishlist_alchemy_codes`).
    pub fn set_wishlist(&self, owner: &str, equipment_codes: Vec<String>, alchemy_codes: Vec<String>) {
        *self.wishlist_owner.lock().unwrap() = Some(owner.to_string());
        *self.wishlist_equipment_codes.lock().unwrap() = equipment_codes;
        *self.wishlist_alchemy_codes.lock().unwrap() = alchemy_codes;
    }

    pub fn wishlist_equipment_codes(&self) -> Vec<String> {
        self.wishlist_equipment_codes.lock().unwrap().clone()
    }

    pub fn wishlist_alchemy_codes(&self) -> Vec<String> {
        self.wishlist_alchemy_codes.lock().unwrap().clone()
    }

    /// Queues `flag` for `target`. `EquipUpgrade` flags are deduplicated against whatever's
    /// already pending for that character/slot/item, since `update_bank` re-derives them from
    /// scratch on every bank refresh and would otherwise pile up duplicates before the target
    /// character gets a chance to drain and act on them.
    pub async fn set_flag(&self, target: &str, flag: Flag) {
        let mut flags = self.flags.lock().await;
        let pending = flags.entry(target.to_string()).or_default();

        let already_pending = match &flag.action {
            FlagAction::EquipUpgrade { slot, item_code } => pending.iter().any(|f| {
                matches!(&f.action, FlagAction::EquipUpgrade { slot: s, item_code: c } if s == slot && c == item_code)
            }),
            FlagAction::HealingConsumablesAvailable => pending.iter()
                .any(|f| matches!(f.action, FlagAction::HealingConsumablesAvailable)),
            FlagAction::CraftWishlistGearAvailable => pending.iter()
                .any(|f| matches!(f.action, FlagAction::CraftWishlistGearAvailable)),
            FlagAction::MerchantSummon => pending.iter()
                .any(|f| matches!(f.action, FlagAction::MerchantSummon)),
            FlagAction::RetrieveFromBank(_) => false,
        };
        if already_pending { return; }

        println!("[{}] Flag set (from '{}'): {:?}", crate::ts_char(target), flag.from_character, flag.action);
        pending.push(flag);
    }

    pub async fn drain_flags(&self, character: &str) -> Vec<Flag> {
        self.flags.lock().await
            .remove(character)
            .unwrap_or_default()
    }

    /// Replaces the cached bank snapshot, then flags any character with a ranked-upgrade item
    /// (per the last `optimize_items`/`brute_force_optimal_loadout` run cached in `item_ratings`)
    /// sitting in the bank right now — whether it just got crafted, was already there, or dropped
    /// from a fight.
    pub async fn update_bank(&self, items: Vec<BankItem>) {
        *self.bank.lock().await = items;
        self.flag_bank_upgrades().await;
        self.flag_healing_consumables_restocked().await;
        self.flag_craftable_wishlist_gear().await;
    }

    /// If the bank now holds enough materials to craft at least one wishlisted equipment item
    /// (weaponcrafting/gearcrafting/jewelrycrafting — see `wishlist_equipment_codes`), flags the
    /// character that owns that wishlist so it can go withdraw and craft it. Runs on every bank
    /// refresh regardless of who caused it — "deposited... by this character or another" — and is
    /// safe to re-trigger repeatedly since `set_flag` dedupes `CraftWishlistGearAvailable`.
    async fn flag_craftable_wishlist_gear(&self) {
        let Some(owner) = self.wishlist_owner.lock().unwrap().clone() else { return };
        let codes = self.wishlist_equipment_codes.lock().unwrap().clone();
        if codes.is_empty() { return; }

        let bank = self.bank_snapshot().await;
        let any_craftable = codes.iter().any(|code| {
            self.data.items.iter().find(|i| i.code == *code)
                .and_then(|i| i.craft.as_ref())
                .is_some_and(|craft| craft.items.iter().all(|ing| {
                    bank.iter().find(|b| b.code == ing.code).map(|b| b.quantity).unwrap_or(0) >= ing.quantity
                }))
        });
        if !any_craftable { return; }

        self.set_flag(&owner, Flag {
            from_character: "bank".to_string(),
            action: FlagAction::CraftWishlistGearAvailable,
        }).await;
    }

    /// If a character is waiting on healing consumables (`awaiting_healing_consumables`) and the
    /// bank now genuinely has one, flags them and clears the wait — this is what turns any
    /// character's bank trip into a push notification instead of the waiting character having to
    /// poll.
    async fn flag_healing_consumables_restocked(&self) {
        let Some(name) = self.awaiting_healing_consumables.lock().unwrap().clone() else { return };

        let bank = self.bank_snapshot().await;
        let has_any = bank.iter().any(|b| {
            b.quantity > 0 && self.data.items.iter().find(|i| i.code == b.code).is_some_and(is_healing_consumable)
        });
        if !has_any { return; }

        *self.awaiting_healing_consumables.lock().unwrap() = None;
        self.set_flag(&name, Flag {
            from_character: "bank".to_string(),
            action: FlagAction::HealingConsumablesAvailable,
        }).await;
    }

    async fn flag_bank_upgrades(&self) {
        let bank = self.bank_snapshot().await;
        let ratings = self.item_ratings.lock().unwrap().clone();

        for (char_name, slot_ratings) in ratings {
            let equipped = self.equipped.lock().unwrap().get(&char_name).cloned().unwrap_or_default();
            self.flag_bank_upgrades_for_ratings(&char_name, &slot_ratings, &bank, &equipped).await;
        }
    }

    /// Flags `char_name` for any item in `bank` that's a genuine upgrade over what's *currently*
    /// equipped in that slot — the shared check behind both the all-characters bank-refresh scan
    /// (`flag_bank_upgrades`) and the single-character check run right after a character's ratings
    /// are (re)computed during initialization (`flag_bank_upgrades_for`).
    async fn flag_bank_upgrades_for_ratings(
        &self,
        char_name: &str,
        slot_ratings: &[SlotRating],
        bank: &[BankItem],
        equipped: &HashMap<String, String>,
    ) {
        for r in slot_ratings {
            // `ranked` is relative to leaving the slot *empty*, not to whatever's currently worn
            // — an item can beat nothing while still being a downgrade from current gear (this
            // was a real bug: a character wearing a solid staff got swapped to a worse axe just
            // because the axe was sitting in the bank and rated above zero). Comparing against
            // the current item's own rating within the same ranked list — walking best-first, so
            // the first hit is the best obtainable one, not necessarily rank 1 — fixes that.
            let current_code   = equipped.get(r.slot).map(|s| s.as_str());
            let current_rating = current_slot_rating(&r.ranked, current_code);

            let available = r.ranked.iter()
                .find(|item| {
                    item.rating > current_rating
                        && current_code != Some(item.code.as_str())
                        && bank.iter().any(|b| b.code == item.code && b.quantity > 0)
                });
            let Some(item) = available else { continue };

            self.set_flag(char_name, Flag {
                from_character: "bank".to_string(),
                action: FlagAction::EquipUpgrade { slot: r.slot.to_string(), item_code: item.code.clone() },
            }).await;
        }
    }

    /// Checks `char_name`'s cached ratings against the bank right now and flags any upgrade
    /// sitting there. Meant to run right after `set_item_ratings` during program initialization:
    /// the bank is loaded (and its one-time upgrade scan run) before any character has ratings
    /// cached, so without this, an upgrade already sitting in the bank at startup would go
    /// undetected until the next deposit happens to re-trigger `flag_bank_upgrades` for everyone.
    pub async fn flag_bank_upgrades_for(&self, char_name: &str) {
        let bank = self.bank_snapshot().await;
        let Some(slot_ratings) = self.item_ratings.lock().unwrap().get(char_name).cloned() else { return };
        let equipped = self.equipped.lock().unwrap().get(char_name).cloned().unwrap_or_default();
        self.flag_bank_upgrades_for_ratings(char_name, &slot_ratings, &bank, &equipped).await;
    }

    pub async fn bank_quantity(&self, code: &str) -> i32 {
        self.bank.lock().await
            .iter()
            .find(|i| i.code == code)
            .map(|i| i.quantity)
            .unwrap_or(0)
    }

    pub async fn bank_snapshot(&self) -> Vec<BankItem> {
        self.bank.lock().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimize::RankedItem;

    fn blank_character() -> Character {
        Character {
            name: "test".into(), account: "test".into(), skin: "".into(),
            level: 20, xp: 0, max_xp: 0, gold: 0, speed: 0,
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

    /// The whole point of exposing a ranked list instead of just the winner: if the top-ranked
    /// item for a slot isn't actually obtainable, callers should be able to walk down to the next
    /// one that is. This exercises exactly that against `GameState::flag_bank_upgrades`.
    #[tokio::test]
    async fn bank_flagging_falls_back_to_first_available_ranked_item() {
        let data = GameData {
            monsters: vec![],
            items: vec![],
            resources: vec![],
            maps: vec![],
            craftable_equip: vec![],
        };

        let ranked = vec![
            RankedItem { code: "amulet_rank1".into(), rating: 300.0 },
            RankedItem { code: "amulet_rank2".into(), rating: 200.0 },
            RankedItem { code: "amulet_rank3".into(), rating: 100.0 },
        ];
        let slot_ratings = vec![SlotRating { slot: "amulet", category: "amulet", ranked }];

        let state = GameState::new(data);
        state.set_item_ratings("char1", slot_ratings, &blank_character());

        // Only the 3rd-ranked item is actually in the bank right now.
        state.update_bank(vec![BankItem { code: "amulet_rank3".into(), quantity: 1 }]).await;

        let flags = state.drain_flags("char1").await;
        assert_eq!(flags.len(), 1);
        match &flags[0].action {
            FlagAction::EquipUpgrade { slot, item_code } => {
                assert_eq!(slot, "amulet");
                assert_eq!(item_code, "amulet_rank3");
            }
            other => panic!("expected EquipUpgrade, got {other:?}"),
        }
    }

    /// A spare copy of an item the character already has equipped shouldn't keep re-triggering
    /// an "upgrade" flag — this was a real bug: withdrawing one copy of a multi-copy bank stack
    /// left enough behind to look like a fresh upgrade on the very next bank refresh.
    #[tokio::test]
    async fn bank_flagging_skips_item_already_equipped() {
        let data = GameData {
            monsters: vec![],
            items: vec![],
            resources: vec![],
            maps: vec![],
            craftable_equip: vec![],
        };

        let ranked = vec![
            RankedItem { code: "amulet_rank1".into(), rating: 300.0 },
            RankedItem { code: "amulet_rank2".into(), rating: 200.0 },
        ];
        let slot_ratings = vec![SlotRating { slot: "amulet", category: "amulet", ranked }];

        let mut character = blank_character();
        character.amulet_slot = "amulet_rank1".into(); // already wearing the #1 pick

        let state = GameState::new(data);
        state.set_item_ratings("char1", slot_ratings, &character);

        // A spare copy of the already-equipped item is still sitting in the bank.
        state.update_bank(vec![BankItem { code: "amulet_rank1".into(), quantity: 1 }]).await;

        let flags = state.drain_flags("char1").await;
        assert!(flags.is_empty(), "should not re-flag an item already equipped in that slot");
    }

    /// A bank item that beats *leaving the slot empty* but is still worse than what's currently
    /// equipped must never get flagged as an "upgrade" — this was a real bug: the flagging logic
    /// only ever checked "is this item's rating positive" (relative to empty), so a weak item
    /// sitting in the bank could displace strictly better gear the character already had on.
    #[tokio::test]
    async fn bank_flagging_refuses_downgrade_from_current_gear() {
        let data = GameData {
            monsters: vec![],
            items: vec![],
            resources: vec![],
            maps: vec![],
            craftable_equip: vec![],
        };

        let ranked = vec![
            RankedItem { code: "good_weapon".into(), rating: 500.0 },
            RankedItem { code: "weak_weapon".into(), rating: 50.0 }, // beats empty, but not current gear
        ];
        let slot_ratings = vec![SlotRating { slot: "weapon", category: "weapon", ranked }];

        let mut character = blank_character();
        character.weapon_slot = "good_weapon".into();

        let state = GameState::new(data);
        state.set_item_ratings("char1", slot_ratings, &character);

        state.update_bank(vec![BankItem { code: "weak_weapon".into(), quantity: 1 }]).await;

        let flags = state.drain_flags("char1").await;
        assert!(flags.is_empty(), "should not flag a downgrade from currently-equipped gear");
    }

    fn make_craft_item(code: &str, ingredients: Vec<(&str, i32)>) -> crate::types::Item {
        crate::types::Item {
            name: code.into(), code: code.into(), level: 1, item_type: "weapon".into(),
            subtype: "".into(), description: "".into(), conditions: vec![], effects: vec![],
            craft: Some(crate::types::CraftInfo {
                skill: Some("weaponcrafting".into()),
                level: None,
                items: ingredients.into_iter().map(|(c, q)| crate::types::CraftIngredient { code: c.into(), quantity: q }).collect(),
                quantity: 1,
            }),
            tradeable: true, recyclable: false,
        }
    }

    /// The bank-deposit craft trigger should only fire once the bank genuinely has enough of
    /// *every* ingredient for at least one wishlisted equipment item, and only for whichever
    /// character owns that wishlist.
    #[tokio::test]
    async fn craft_wishlist_gear_flag_fires_only_once_fully_craftable() {
        let data = GameData {
            monsters: vec![], resources: vec![], maps: vec![], craftable_equip: vec![],
            items: vec![make_craft_item("iron_sword", vec![("iron_bar", 2), ("wood", 1)])],
        };
        let state = GameState::new(data);
        state.set_wishlist("crafter1", vec!["iron_sword".to_string()], vec![]);

        // Only one of the two ingredients present — must not fire yet.
        state.update_bank(vec![BankItem { code: "iron_bar".into(), quantity: 2 }]).await;
        assert!(state.drain_flags("crafter1").await.is_empty(), "should not fire with a missing ingredient");

        // Both ingredients now present in sufficient quantity — should fire.
        state.update_bank(vec![
            BankItem { code: "iron_bar".into(), quantity: 2 },
            BankItem { code: "wood".into(), quantity: 1 },
        ]).await;

        let flags = state.drain_flags("crafter1").await;
        assert_eq!(flags.len(), 1);
        assert!(matches!(flags[0].action, FlagAction::CraftWishlistGearAvailable));

        // Some other character was never registered as the wishlist owner and must never receive it.
        assert!(state.drain_flags("someone_else").await.is_empty());
    }

    /// Repeated bank updates that keep the condition true shouldn't pile up duplicate flags before
    /// the owning character has a chance to drain them.
    #[tokio::test]
    async fn craft_wishlist_gear_flag_is_deduplicated() {
        let data = GameData {
            monsters: vec![], resources: vec![], maps: vec![], craftable_equip: vec![],
            items: vec![make_craft_item("iron_sword", vec![("iron_bar", 2)])],
        };
        let state = GameState::new(data);
        state.set_wishlist("crafter1", vec!["iron_sword".to_string()], vec![]);

        state.update_bank(vec![BankItem { code: "iron_bar".into(), quantity: 5 }]).await;
        state.update_bank(vec![BankItem { code: "iron_bar".into(), quantity: 5 }]).await;
        state.update_bank(vec![BankItem { code: "iron_bar".into(), quantity: 5 }]).await;

        let flags = state.drain_flags("crafter1").await;
        assert_eq!(flags.len(), 1, "repeated triggers of the same still-true condition should not pile up");
    }
}
