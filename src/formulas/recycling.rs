use std::collections::HashMap;

use crate::types::{CraftInfo, Item};

/// Only weaponcrafting/gearcrafting/jewelrycrafting items are ever recyclable for wishlist
/// purposes — alchemy items are never recyclable, regardless of `item.recyclable`.
pub fn is_recyclable_for_wishlist(item: &Item, skill: &str) -> bool {
    item.recyclable && matches!(skill, "weaponcrafting" | "gearcrafting" | "jewelrycrafting")
}

fn recipe_ingredient_count(craft: &CraftInfo) -> i32 {
    craft.items.iter().map(|i| i.quantity).sum()
}

/// Non-enhanced (free) recycling return count for one recycled item: `floor((n-1)/5) + 1`, minimum
/// 1, where `n` is the total ingredient quantity in the recipe (not distinct ingredient types).
pub fn recycle_return_quantity(craft: &CraftInfo) -> i32 {
    let n = recipe_ingredient_count(craft).max(1);
    (n - 1) / 5 + 1
}

/// Expected quantity of each ingredient returned by recycling one crafted item, distributing
/// `recycle_return_quantity`'s total proportionally across the recipe's ingredients by their
/// quantity share (the game picks materials to return at random from the recipe, so this is an
/// expected-value estimate for wishlist projections, not a guaranteed per-ingredient amount).
pub fn recycle_returns_per_ingredient(craft: &CraftInfo) -> HashMap<String, f64> {
    let total = recipe_ingredient_count(craft).max(1) as f64;
    let returned = recycle_return_quantity(craft) as f64;
    craft.items.iter()
        .map(|ing| (ing.code.clone(), returned * ing.quantity as f64 / total))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CraftIngredient;

    fn make_craft_item(code: &str, level: i32, skill: &str, ingredients: Vec<(&str, i32)>, recyclable: bool) -> Item {
        Item {
            name: code.into(), code: code.into(), level, item_type: "weapon".into(),
            subtype: "".into(), description: "".into(), conditions: vec![],
            effects: vec![],
            craft: Some(CraftInfo {
                skill: Some(skill.into()),
                level: None,
                items: ingredients.into_iter().map(|(c, q)| CraftIngredient { code: c.into(), quantity: q }).collect(),
                quantity: 1,
            }),
            tradeable: true,
            recyclable,
        }
    }

    #[test]
    fn recycle_return_quantity_crosses_floor_boundary() {
        let craft_n = |n: i32| CraftInfo {
            skill: Some("weaponcrafting".into()), level: None,
            items: vec![CraftIngredient { code: "x".into(), quantity: n }], quantity: 1,
        };
        assert_eq!(recycle_return_quantity(&craft_n(1)), 1);
        assert_eq!(recycle_return_quantity(&craft_n(5)), 1);
        assert_eq!(recycle_return_quantity(&craft_n(6)), 2);
        assert_eq!(recycle_return_quantity(&craft_n(10)), 2);
        assert_eq!(recycle_return_quantity(&craft_n(11)), 3);
    }

    #[test]
    fn is_recyclable_for_wishlist_excludes_alchemy() {
        let potion = make_craft_item("potion", 5, "alchemy", vec![("herb", 2)], true);
        assert!(!is_recyclable_for_wishlist(&potion, "alchemy"));

        let sword = make_craft_item("sword", 5, "weaponcrafting", vec![("bar", 2)], true);
        assert!(is_recyclable_for_wishlist(&sword, "weaponcrafting"));

        let sword_not_flagged = make_craft_item("sword2", 5, "weaponcrafting", vec![("bar", 2)], false);
        assert!(!is_recyclable_for_wishlist(&sword_not_flagged, "weaponcrafting"));
    }
}
