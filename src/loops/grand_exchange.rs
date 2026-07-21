//! Grand Exchange purchase *planning* only — this module figures out what's worth buying and at
//! what price, but never posts an actual buy order. Wiring the resulting plan up to real
//! `POST /my/{name}/action/grandexchange/buy` calls (and to triggers other than program startup)
//! is a later step; for now `plan_ge_purchases` is read-only (it queries orders, never posts one)
//! and its result is only ever printed.

use std::collections::{HashMap, HashSet};

use reqwest::Client;

use crate::api::{get_ge_sell_orders, GameData};
use crate::flags::GameState;
use crate::formulas::calculate_crafting_xp;
use crate::optimize::{current_slot_rating, skill_level, skill_xp_progress};
use crate::types::{Character, CraftInfo, GEOrder, Item};

use super::crafting_plan::{build_wishlist, xp_to_next_tier, WISHLIST_SKILLS};

/// Cap on how many purchase rounds `plan_wishlist_purchases` will run — a defensive bound, not an
/// expected steady-state count. Each round batches as many crafts as it can at the current
/// marginal price (see `batch_capacity_at_current_price`), so in practice this only gets
/// approached if there are an unusually large number of distinct price-tier transitions to walk
/// through, not one per craft.
const MAX_WISHLIST_PURCHASE_ROUNDS: u32 = 5_000;

// ── Order book / ledger ──────────────────────────────────────────────────────

/// One item's Grand Exchange sell-side order book: remaining (price, quantity) tiers, sorted
/// ascending by price, plus a running tally of what's been claimed against it during this
/// planning pass (bookkeeping only — nothing here is an actual API call).
#[derive(Clone, Default)]
struct GEOrderBook {
    tiers: Vec<(i32, i32)>, // (price, remaining_quantity), ascending by price
    claimed_qty: i32,
    claimed_cost: i32,
}

impl GEOrderBook {
    fn from_orders(orders: &[GEOrder]) -> Self {
        let mut tiers: Vec<(i32, i32)> = orders.iter()
            .filter(|o| o.quantity > 0)
            .map(|o| (o.price, o.quantity))
            .collect();
        tiers.sort_by_key(|(price, _)| *price);
        Self { tiers, claimed_qty: 0, claimed_cost: 0 }
    }

    /// Claims up to `qty` units from the cheapest remaining tiers. Returns (units obtained, gold
    /// cost) — `obtained` is less than `qty` if the book doesn't have that much left at all.
    fn claim(&mut self, qty: i32) -> (i32, i32) {
        let mut remaining = qty;
        let mut cost = 0;
        let mut obtained = 0;

        for (price, avail) in self.tiers.iter_mut() {
            if remaining <= 0 { break; }
            let take = remaining.min(*avail);
            if take <= 0 { continue; }
            cost += take * *price;
            *avail -= take;
            remaining -= take;
            obtained += take;
        }

        self.claimed_qty += obtained;
        self.claimed_cost += cost;
        (obtained, cost)
    }
}

/// A snapshot of Grand Exchange sell orders for every item code this planning pass cares about,
/// fetched once up front (see `fetch_ge_market`). Cloned freely during recursive cost comparisons
/// (see `Ledger`) — it only ever holds as many books as candidate items were queried for, not the
/// whole game catalog.
#[derive(Clone, Default)]
struct GEMarket {
    books: HashMap<String, GEOrderBook>,
}

impl GEMarket {
    fn claim(&mut self, code: &str, qty: i32) -> (i32, i32) {
        self.books.get_mut(code).map(|b| b.claim(qty)).unwrap_or((0, 0))
    }

    /// Everything actually claimed by this planning pass, as (code, quantity, gold spent) —
    /// derived from each book's running tally rather than tracked separately alongside it.
    fn purchases(&self) -> Vec<(String, i32, i32)> {
        self.books.iter()
            .filter(|(_, b)| b.claimed_qty > 0)
            .map(|(code, b)| (code.clone(), b.claimed_qty, b.claimed_cost))
            .collect()
    }
}

/// Combined free-materials (bank) and paid-materials (Grand Exchange) supply, threaded through
/// planning so nothing gets double-claimed across different upgrade candidates or wishlist
/// recipes competing for the same scarce bank stock or cheap sell orders — the same role
/// `remaining_supply: HashMap<String, i32>` plays in `crafting_plan.rs`, extended with a paid
/// fallback.
#[derive(Clone, Default)]
struct Ledger {
    bank: HashMap<String, i32>,
    market: GEMarket,
}

// ── Recursive cheapest-acquisition pricing (upgrades only) ──────────────────

/// Cheapest gold cost to obtain `qty` more of `code`, preferring free bank stock, then buying it
/// directly on the Grand Exchange, then crafting it from its own ingredients (each priced the same
/// way, recursively) — whichever of buy-direct or craft-from-ingredients is cheaper, once bank
/// stock has covered as much as it can for free. Mutates `ledger` to reflect whichever path was
/// actually used, but *only* if the full `qty` was obtainable — a failed attempt (`None`) leaves
/// `ledger` completely untouched, so a caller trying the next-ranked alternative never sees a
/// partially-consumed ledger from a dead end. `None` if `qty` can't be fully sourced by any
/// combination of bank stock, buying, and crafting (including missing the crafting-skill level
/// for it, or it simply not existing on the Grand Exchange at all).
///
/// This only ever compares "buy the whole remaining shortfall" against "craft the whole remaining
/// shortfall" as complete alternatives — it doesn't explore mixing the two (e.g. buying the first
/// few cheap Grand Exchange units and crafting the rest once those orders run dry). That mix could
/// occasionally be cheaper, but the two-whole-branches comparison is what's implemented here.
fn cheapest_acquisition_cost(
    code: &str,
    qty: i32,
    data: &GameData,
    character: &Character,
    ledger: &mut Ledger,
) -> Option<i32> {
    if qty <= 0 { return Some(0); }

    let buy_trial: Option<(i32, Ledger)> = {
        let mut trial = ledger.clone();
        let free = trial.bank.get(code).copied().unwrap_or(0).min(qty);
        *trial.bank.entry(code.to_string()).or_insert(0) -= free;
        let remaining = qty - free;
        if remaining == 0 {
            Some((0, trial))
        } else {
            let (obtained, cost) = trial.market.claim(code, remaining);
            (obtained == remaining).then_some((cost, trial))
        }
    };

    let craft_trial: Option<(i32, Ledger)> = (|| {
        let mut trial = ledger.clone();
        let free = trial.bank.get(code).copied().unwrap_or(0).min(qty);
        *trial.bank.entry(code.to_string()).or_insert(0) -= free;
        let remaining = qty - free;
        if remaining == 0 { return Some((0, trial)); }

        let item = data.items.iter().find(|i| i.code == code)?;
        let craft = item.craft.as_ref()?;
        let skill = craft.skill.as_deref()?;
        if craft.level.is_some_and(|req| skill_level(character, skill) < req) { return None; }

        let output_per_craft = craft.quantity.max(1);
        let crafts_needed = (remaining as f64 / output_per_craft as f64).ceil() as i32;

        let mut total = 0i32;
        for ing in &craft.items {
            let ing_qty = ing.quantity * crafts_needed;
            total += cheapest_acquisition_cost(&ing.code, ing_qty, data, character, &mut trial)?;
        }
        Some((total, trial))
    })();

    let (cost, resolved) = match (buy_trial, craft_trial) {
        (Some((b, bt)), Some((c, ct))) => if c < b { (c, ct) } else { (b, bt) },
        (Some(bt), None) => bt,
        (None, Some(ct)) => ct,
        (None, None) => return None,
    };

    *ledger = resolved;
    Some(cost)
}

// ── Upgrade-purchase planning ─────────────────────────────────────────────────

/// One committed equipment-upgrade purchase: which character/slot it resolves, and what it cost
/// to obtain via `cheapest_acquisition_cost` (bank + Grand Exchange, recursively through crafting
/// ingredients).
struct UpgradePurchase {
    char_name: String,
    slot: String,
    code: String,
    rating: f64,
    cost: i32,
}

/// Global-priority equipment-upgrade purchase planner — the Grand-Exchange-aware counterpart to
/// `crafting_plan::allocate_upgrade_crafts`. For every character's every slot, walks *every*
/// ranked alternative that beats what's currently equipped there (best-rated first, exactly like
/// `allocate_upgrade_crafts`), and commits the first one whose full cost — cheapest combination of
/// free bank stock, buying it outright on the Grand Exchange, or crafting it from ingredients
/// (recursively priced) — fits within the remaining `gold_budget`; lower-ranked alternatives for
/// that slot are skipped once it resolves, and a candidate that's unaffordable (rather than
/// unobtainable) still leaves room for a cheaper, lower-rated alternative to win the slot instead.
/// `character` is the crafting character (used for crafting-skill-level checks along the way,
/// regardless of which character will end up wearing the result) — same convention as
/// `allocate_upgrade_crafts`. Mutates `ledger` and `gold_budget` as purchases are committed, so a
/// later (lower-priority) slot never double-spends gold or bank/market stock a higher-priority
/// upgrade already claimed; a candidate that turns out unaffordable never touches `ledger` at all
/// (evaluated against a throwaway clone first).
fn plan_upgrade_purchases(
    state: &GameState,
    character: &Character,
    ledger: &mut Ledger,
    gold_budget: &mut i32,
) -> Vec<UpgradePurchase> {
    struct Step { char_name: String, slot: String, code: String, rating: f64 }

    let mut steps: Vec<Step> = Vec::new();
    for (char_name, ratings) in state.all_item_ratings() {
        let equipped = state.equipped_snapshot(&char_name);
        for r in &ratings {
            let current_code   = equipped.get(r.slot).map(|s| s.as_str());
            let current_rating = current_slot_rating(&r.ranked, current_code);

            for item in &r.ranked {
                if item.rating <= current_rating { continue; }
                if current_code == Some(item.code.as_str()) { continue; }
                steps.push(Step {
                    char_name: char_name.clone(),
                    slot: r.slot.to_string(),
                    code: item.code.clone(),
                    rating: item.rating,
                });
            }
        }
    }

    steps.sort_by(|a, b| b.rating.partial_cmp(&a.rating).unwrap_or(std::cmp::Ordering::Equal));

    let mut resolved: HashSet<(String, String)> = HashSet::new();
    let mut purchases = Vec::new();

    for step in steps {
        let slot_key = (step.char_name.clone(), step.slot.clone());
        if resolved.contains(&slot_key) { continue; }

        let mut trial = ledger.clone();
        let Some(cost) = cheapest_acquisition_cost(&step.code, 1, &state.data, character, &mut trial) else { continue };
        if cost > *gold_budget { continue; }

        *ledger = trial;
        *gold_budget -= cost;
        resolved.insert(slot_key);
        purchases.push(UpgradePurchase { char_name: step.char_name, slot: step.slot, code: step.code, rating: step.rating, cost });
    }

    purchases
}

// ── Wishlist-purchase planning (XP/gold maximization) ────────────────────────

/// One committed wishlist purchase: how many crafts of `code` under `skill` were paid for in
/// total, the combined gold spent, and the combined XP gained.
struct WishlistPurchase {
    code: String,
    skill: String,
    crafts: i32,
    cost: i32,
    xp: f64,
}

/// The largest `n` such that buying `n` more crafts' worth of `craft`'s ingredients would cost
/// exactly `n` times what buying one more craft costs right now, for every ingredient — i.e.
/// before any ingredient's free bank stock runs out, or its cheapest Grand Exchange tier is
/// exhausted, and the marginal price for the *next* one would step up. Read-only (only peeks
/// `ledger`, via the same bank-then-cheapest-tier order `cheapest_acquisition_cost` prices with).
/// Always at least 1 — callers only reach for this once they've already confirmed one more craft
/// is affordable, so it's never used to justify a batch smaller than what's already committed to.
///
/// This deliberately doesn't chase exact optimality at a boundary that falls mid-craft (e.g. free
/// bank stock covers 5 of an ingredient but a craft needs 2, straddling the free/paid line) —
/// integer-dividing by the ingredient's per-craft quantity there rounds the batch down rather than
/// up, so at worst this stops a little earlier than the true boundary and re-evaluates next round,
/// never assumes a flat price across a boundary that wasn't actually flat.
fn batch_capacity_at_current_price(craft: &CraftInfo, ledger: &Ledger) -> i32 {
    craft.items.iter()
        .filter(|ing| ing.quantity > 0)
        .map(|ing| {
            let bank_free = ledger.bank.get(&ing.code).copied().unwrap_or(0);
            let flat_units = if bank_free > 0 {
                bank_free
            } else {
                // The cheapest tier with anything actually left in it — a fully-claimed tier
                // stays in `tiers` at quantity 0 rather than being removed (see `GEOrderBook`),
                // so skipping straight to `tiers.first()` here would wrongly read a stale 0.
                ledger.market.books.get(&ing.code)
                    .and_then(|b| b.tiers.iter().find(|(_, qty)| *qty > 0))
                    .map(|(_, qty)| *qty)
                    .unwrap_or(0)
            };
            flat_units / ing.quantity
        })
        .min()
        .unwrap_or(1)
        .max(1)
}

/// Greedily buys wishlist-recipe ingredients (bank-free first, then cheapest Grand Exchange orders
/// — no recursion into sub-ingredients, unlike upgrade purchases, since a recipe's own direct
/// ingredients are the thing actually being bought *to craft*, not itself a buy-vs-craft choice)
/// to maximize total crafting XP per gold spent, without exceeding `gold_budget`. Each round picks
/// the single best-ratio recipe (freely mixing skills as their marginal XP/gold ratio dictates),
/// then buys as many crafts of it as fit at the *current* marginal price in one batch (see
/// `batch_capacity_at_current_price`) — re-evaluating from scratch only once that price would
/// actually change, rather than one craft at a time — stopping a skill once its next-tier XP gap
/// is closed by *any* combination of its alternative recipes. That shared `remaining_xp` budget
/// *per skill* (not per recipe) is exactly what avoids double-counting: if 15 lizard scales or 30
/// mercury bars would each independently finish the tier, buying some mix of both stops the moment
/// the shared gap closes, never both in full.
fn plan_wishlist_purchases(
    state: &GameState,
    character: &Character,
    ledger: &mut Ledger,
    gold_budget: &mut i32,
) -> Vec<WishlistPurchase> {
    let mut remaining_xp: HashMap<&'static str, f64> = WISHLIST_SKILLS.iter()
        .map(|&skill| {
            let (level, xp, max_xp) = skill_xp_progress(character, skill);
            (skill, xp_to_next_tier(level, xp, max_xp) as f64)
        })
        .collect();

    let mut purchases: HashMap<String, WishlistPurchase> = HashMap::new();

    for _ in 0..MAX_WISHLIST_PURCHASE_ROUNDS {
        if *gold_budget <= 0 { break; }

        // (item, craft, skill, cost of one more craft, xp of one craft, ratio)
        let mut best: Option<(&Item, &CraftInfo, &'static str, i32, f64, f64)> = None;

        for &skill in &WISHLIST_SKILLS {
            if remaining_xp.get(skill).copied().unwrap_or(0.0) <= 0.0 { continue; }
            let level = skill_level(character, skill);

            for item in &state.data.items {
                let Some(craft) = &item.craft else { continue };
                if craft.skill.as_deref() != Some(skill) { continue; }
                if craft.level.is_some_and(|req| level < req) { continue; }

                let xp = calculate_crafting_xp(level, item, skill, character.wisdom);
                if xp <= 0.0 { continue; }

                let mut trial = ledger.clone();
                let mut cost = 0i32;
                let mut affordable = true;
                for ing in &craft.items {
                    match cheapest_acquisition_cost(&ing.code, ing.quantity, &state.data, character, &mut trial) {
                        Some(c) => cost += c,
                        None => { affordable = false; break; }
                    }
                }
                if !affordable || cost > *gold_budget { continue; }

                let ratio = xp / cost.max(1) as f64;
                if best.as_ref().is_none_or(|(_, _, _, _, _, best_ratio)| ratio > *best_ratio) {
                    best = Some((item, craft, skill, cost, xp, ratio));
                }
            }
        }

        let Some((item, craft, skill, cost_of_one, xp_of_one, _)) = best else { break };

        let price_cap = batch_capacity_at_current_price(craft, ledger);
        let gold_cap = (*gold_budget / cost_of_one.max(1)).max(1);
        let xp_cap = (remaining_xp[skill] / xp_of_one).ceil().max(1.0) as i32;
        let batch = price_cap.min(gold_cap).min(xp_cap).max(1);

        let mut batch_cost = 0i32;
        for ing in &craft.items {
            batch_cost += cheapest_acquisition_cost(&ing.code, ing.quantity * batch, &state.data, character, ledger)
                .expect("batch was sized against what's already known affordable and obtainable this round");
        }
        let batch_xp = xp_of_one * batch as f64;

        *gold_budget -= batch_cost;
        *remaining_xp.entry(skill).or_insert(0.0) -= batch_xp;

        purchases.entry(item.code.clone())
            .and_modify(|p| { p.crafts += batch; p.cost += batch_cost; p.xp += batch_xp; })
            .or_insert_with(|| WishlistPurchase { code: item.code.clone(), skill: skill.to_string(), crafts: batch, cost: batch_cost, xp: batch_xp });
    }

    purchases.into_values().collect()
}

// ── Fetching and orchestration ────────────────────────────────────────────────

/// Every item code reachable from `code` by recursing into its own crafting ingredients
/// (including `code` itself) — the set of Grand Exchange listings an upgrade candidate's
/// cheapest-acquisition search might need prices for.
fn collect_craft_tree_codes(code: &str, data: &GameData, seen: &mut HashSet<String>) {
    if !seen.insert(code.to_string()) { return; }
    let Some(item) = data.items.iter().find(|i| i.code == code) else { return };
    let Some(craft) = &item.craft else { return };
    for ing in &craft.items {
        collect_craft_tree_codes(&ing.code, data, seen);
    }
}

/// Every item code the Grand-Exchange-aware planner might need a price for: every ranked
/// equipment-upgrade candidate across every character (recursively through its own crafting
/// ingredients, since an upgrade might be bought directly or crafted from bought materials), plus
/// every wishlist recipe's *direct* ingredients (wishlist purchases don't recurse further — see
/// `plan_wishlist_purchases`).
fn collect_ge_query_codes(state: &GameState, character: &Character) -> HashSet<String> {
    let mut codes = HashSet::new();

    for (_char_name, ratings) in state.all_item_ratings() {
        for r in &ratings {
            for item in &r.ranked {
                collect_craft_tree_codes(&item.code, &state.data, &mut codes);
            }
        }
    }

    for entry in build_wishlist(state, character) {
        if let Some(craft) = state.data.items.iter().find(|i| i.code == entry.code).and_then(|i| i.craft.as_ref()) {
            for ing in &craft.items {
                codes.insert(ing.code.clone());
            }
        }
    }

    codes
}

/// Queries the Grand Exchange for sell orders of every item code this planning pass might need a
/// price for (see `collect_ge_query_codes`) — one request per code, repeated as many times as
/// there are candidate codes, since the endpoint only filters by a single item code per call.
async fn fetch_ge_market(client: &Client, name: &str, state: &GameState, character: &Character) -> GEMarket {
    let codes = collect_ge_query_codes(state, character);
    println!("[{}] Querying Grand Exchange for {} item code(s)...", crate::ts_char(name), codes.len());

    let mut books = HashMap::new();
    for code in codes {
        match get_ge_sell_orders(client, &code).await {
            Ok(orders) if !orders.is_empty() => { books.insert(code, GEOrderBook::from_orders(&orders)); }
            Ok(_) => {}
            Err(e) => eprintln!("[{}] Failed to fetch Grand Exchange orders for {}: {}", crate::ts_char(name), code, e),
        }
    }

    GEMarket { books }
}

/// The full computed (not yet executed) Grand Exchange purchase plan — see the module doc comment.
pub struct GEPurchasePlan {
    upgrades: Vec<UpgradePurchase>,
    wishlist: Vec<WishlistPurchase>,
    /// Leaf-level breakdown of what actually got claimed from the Grand Exchange order book
    /// across both phases combined — (code, quantity, gold spent) — for a transparent view of
    /// where the gold in `total_cost` actually went, distinct from `upgrades`/`wishlist`'s
    /// higher-level "what was this purchase *for*" view.
    market_purchases: Vec<(String, i32, i32)>,
    total_cost: i32,
}

/// Computes the optimal Grand Exchange purchase plan for `character` (the crafting character)
/// given `available_gold`: first spends toward the highest-rated obtainable equipment upgrade for
/// every character/slot (buying materials or the item itself, recursively, cheapest path first —
/// see `plan_upgrade_purchases`), then spends whatever gold remains maximizing crafting-skill
/// XP/gold via wishlist recipes (see `plan_wishlist_purchases`). Queries the Grand Exchange and
/// combines it with the cached bank snapshot, but — per the module doc comment — doesn't place
/// any actual buy orders.
pub async fn plan_ge_purchases(client: &Client, state: &GameState, character: &Character, available_gold: i32) -> GEPurchasePlan {
    let market = fetch_ge_market(client, &character.name, state, character).await;

    let bank = state.bank_snapshot().await;
    let mut bank_supply: HashMap<String, i32> = HashMap::new();
    for b in &bank {
        *bank_supply.entry(b.code.clone()).or_insert(0) += b.quantity;
    }

    let mut ledger = Ledger { bank: bank_supply, market };
    let mut gold_budget = available_gold;

    let upgrades = plan_upgrade_purchases(state, character, &mut ledger, &mut gold_budget);
    let wishlist = plan_wishlist_purchases(state, character, &mut ledger, &mut gold_budget);
    let market_purchases = ledger.market.purchases();

    GEPurchasePlan { upgrades, wishlist, market_purchases, total_cost: available_gold - gold_budget }
}

/// Prints a human-readable summary of `plan` — the only consumer of a `GEPurchasePlan` for now,
/// since executing it is a later step.
pub fn print_ge_purchase_plan(name: &str, plan: &GEPurchasePlan) {
    println!("[{}] Grand Exchange purchase plan:", crate::ts_char(name));

    if plan.upgrades.is_empty() {
        println!("[{}]   Upgrades: none obtainable/affordable right now.", crate::ts_char(name));
    } else {
        for u in &plan.upgrades {
            println!(
                "[{}]   Upgrade: {} for {} (slot {}, rating {:+.2}) — {} gold",
                crate::ts_char(name), u.code, u.char_name, u.slot, u.rating, u.cost
            );
        }
    }

    if plan.wishlist.is_empty() {
        println!("[{}]   Wishlist: nothing affordable right now.", crate::ts_char(name));
    } else {
        for w in &plan.wishlist {
            println!(
                "[{}]   Wishlist: {}x {} ({}) — {} gold, {:.0} XP",
                crate::ts_char(name), w.crafts, w.code, w.skill, w.cost, w.xp
            );
        }
    }

    if !plan.market_purchases.is_empty() {
        let mut purchases = plan.market_purchases.clone();
        purchases.sort_by_key(|(_, _, cost)| std::cmp::Reverse(*cost));
        let summary: Vec<String> = purchases.iter().map(|(code, qty, cost)| format!("{}x {} ({} gold)", qty, code, cost)).collect();
        println!("[{}]   Bought from Grand Exchange: {}", crate::ts_char(name), summary.join(", "));
    }

    println!("[{}]   Total: {} gold", crate::ts_char(name), plan.total_cost);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::optimize::{RankedItem, SlotRating};
    use crate::types::{CraftIngredient, CraftInfo};

    fn order(price: i32, qty: i32) -> GEOrder {
        GEOrder { id: "1".into(), order_type: "sell".into(), account: "seller".into(), code: "x".into(), quantity: qty, price, created_at: Utc::now() }
    }

    fn blank_character() -> Character {
        Character {
            name: "test".into(), account: "test".into(), skin: "".into(),
            level: 1, xp: 0, max_xp: 0, gold: 0, speed: 0,
            mining_level: 1, mining_xp: 0, mining_max_xp: 0,
            woodcutting_level: 1, woodcutting_xp: 0, woodcutting_max_xp: 0,
            fishing_level: 1, fishing_xp: 0, fishing_max_xp: 0,
            weaponcrafting_level: 1, weaponcrafting_xp: 0, weaponcrafting_max_xp: 150,
            gearcrafting_level: 1, gearcrafting_xp: 0, gearcrafting_max_xp: 150,
            jewelrycrafting_level: 1, jewelrycrafting_xp: 0, jewelrycrafting_max_xp: 150,
            cooking_level: 1, cooking_xp: 0, cooking_max_xp: 0,
            alchemy_level: 1, alchemy_xp: 0, alchemy_max_xp: 150,
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

    fn craft_item(code: &str, skill: &str, level_req: Option<i32>, ingredients: Vec<(&str, i32)>, output_qty: i32) -> Item {
        Item {
            name: code.into(), code: code.into(), level: 1, item_type: "resource".into(),
            subtype: "".into(), description: "".into(), conditions: vec![], effects: vec![],
            craft: Some(CraftInfo {
                skill: Some(skill.into()),
                level: level_req,
                items: ingredients.into_iter().map(|(c, q)| CraftIngredient { code: c.into(), quantity: q }).collect(),
                quantity: output_qty,
            }),
            tradeable: true,
            recyclable: false,
        }
    }

    fn blank_data(items: Vec<Item>) -> GameData {
        GameData { monsters: vec![], items, resources: vec![], maps: vec![], craftable_equip: vec![] }
    }

    // ── GEOrderBook ──────────────────────────────────────────────────────────

    #[test]
    fn order_book_claims_cheapest_tiers_first() {
        let mut book = GEOrderBook::from_orders(&[order(10, 5), order(5, 3), order(20, 100)]);

        let (obtained, cost) = book.claim(6);

        // 3 units @5 (=15) + 3 units @10 (=30) = 45, cheapest tiers exhausted first.
        assert_eq!(obtained, 6);
        assert_eq!(cost, 45);
        assert_eq!(book.claimed_qty, 6);
        assert_eq!(book.claimed_cost, 45);
    }

    #[test]
    fn order_book_partial_fulfillment_when_insufficient() {
        let mut book = GEOrderBook::from_orders(&[order(5, 3)]);

        let (obtained, cost) = book.claim(10);

        assert_eq!(obtained, 3);
        assert_eq!(cost, 15);
    }

    // ── cheapest_acquisition_cost ────────────────────────────────────────────

    #[test]
    fn cheapest_acquisition_uses_free_bank_stock_before_paying_for_anything() {
        let data = blank_data(vec![]);
        let character = blank_character();
        let mut ledger = Ledger::default();
        ledger.bank.insert("iron_bar".into(), 3);
        ledger.market.books.insert("iron_bar".into(), GEOrderBook::from_orders(&[order(50, 10)]));

        let cost = cheapest_acquisition_cost("iron_bar", 3, &data, &character, &mut ledger).unwrap();
        assert_eq!(cost, 0);
        assert_eq!(*ledger.bank.get("iron_bar").unwrap(), 0);
        assert_eq!(ledger.market.books["iron_bar"].claimed_qty, 0);

        // Bank is now empty — the next unit must be bought.
        let cost2 = cheapest_acquisition_cost("iron_bar", 2, &data, &character, &mut ledger).unwrap();
        assert_eq!(cost2, 100);
    }

    #[test]
    fn cheapest_acquisition_prefers_buying_when_cheaper_than_crafting() {
        let data = blank_data(vec![craft_item("gadget", "weaponcrafting", None, vec![("part", 2)], 1)]);
        let character = blank_character();
        let mut ledger = Ledger::default();
        ledger.market.books.insert("gadget".into(), GEOrderBook::from_orders(&[order(15, 5)]));
        ledger.market.books.insert("part".into(), GEOrderBook::from_orders(&[order(10, 10)])); // crafting would cost 20

        let cost = cheapest_acquisition_cost("gadget", 1, &data, &character, &mut ledger).unwrap();

        assert_eq!(cost, 15);
        assert_eq!(ledger.market.books["gadget"].claimed_qty, 1);
        assert_eq!(ledger.market.books["part"].claimed_qty, 0);
    }

    #[test]
    fn cheapest_acquisition_prefers_crafting_when_cheaper_than_buying() {
        let data = blank_data(vec![craft_item("gadget", "weaponcrafting", None, vec![("part", 2)], 1)]);
        let character = blank_character();
        let mut ledger = Ledger::default();
        ledger.market.books.insert("gadget".into(), GEOrderBook::from_orders(&[order(100, 5)]));
        ledger.market.books.insert("part".into(), GEOrderBook::from_orders(&[order(10, 10)])); // crafting costs 20

        let cost = cheapest_acquisition_cost("gadget", 1, &data, &character, &mut ledger).unwrap();

        assert_eq!(cost, 20);
        assert_eq!(ledger.market.books["part"].claimed_qty, 2);
        assert_eq!(ledger.market.books["gadget"].claimed_qty, 0);
    }

    #[test]
    fn cheapest_acquisition_none_when_unobtainable_and_leaves_ledger_untouched() {
        let data = blank_data(vec![]); // no recipe, and no GE listing set up below
        let character = blank_character();
        let mut ledger = Ledger::default();
        ledger.bank.insert("mythril".into(), 1);

        let result = cheapest_acquisition_cost("mythril", 5, &data, &character, &mut ledger);

        assert!(result.is_none());
        // The 1 free unit from bank must NOT have been consumed by the failed attempt.
        assert_eq!(*ledger.bank.get("mythril").unwrap(), 1);
    }

    #[test]
    fn cheapest_acquisition_respects_crafting_skill_level_requirement() {
        let data = blank_data(vec![craft_item("high_tier_sword", "weaponcrafting", Some(50), vec![("bar", 1)], 1)]);
        let mut character = blank_character();
        character.weaponcrafting_level = 10; // below the level-50 requirement, and no GE listing
        let mut ledger = Ledger::default();
        ledger.bank.insert("bar".into(), 10);

        let result = cheapest_acquisition_cost("high_tier_sword", 1, &data, &character, &mut ledger);

        assert!(result.is_none());
    }

    /// A material shared between two ingredients of the *same* craft (one direct, one via a
    /// sub-crafted component) must not be double-counted as free from the bank for both — this is
    /// exactly the sibling-ledger-sharing correctness the recursive/cloning design is for.
    #[test]
    fn cheapest_acquisition_shares_bank_stock_across_sibling_ingredients() {
        let data = blank_data(vec![
            craft_item("widget", "weaponcrafting", None, vec![("part", 2), ("subassembly", 1)], 1),
            craft_item("subassembly", "weaponcrafting", None, vec![("part", 1)], 1),
        ]);
        let character = blank_character();
        let mut ledger = Ledger::default();
        ledger.bank.insert("part".into(), 2); // enough for only one of the two "part" demands
        ledger.market.books.insert("part".into(), GEOrderBook::from_orders(&[order(10, 100)]));

        let cost = cheapest_acquisition_cost("widget", 1, &data, &character, &mut ledger).unwrap();

        // 3 "part" needed total (2 direct + 1 via subassembly); 2 free from bank, 1 bought @10.
        assert_eq!(cost, 10);
    }

    // ── plan_upgrade_purchases ───────────────────────────────────────────────

    #[test]
    fn plan_upgrade_purchases_falls_back_to_affordable_lower_rated_pick() {
        let data = blank_data(vec![]);
        let state = GameState::new(data);
        let character = blank_character();

        state.set_item_ratings("char_a", vec![SlotRating {
            slot: "weapon", category: "weapon",
            ranked: vec![
                RankedItem { code: "expensive_sword".into(), rating: 900.0 },
                RankedItem { code: "cheap_sword".into(), rating: 100.0 },
            ],
        }], &character);

        let mut ledger = Ledger::default();
        ledger.market.books.insert("expensive_sword".into(), GEOrderBook::from_orders(&[order(1000, 1)]));
        ledger.market.books.insert("cheap_sword".into(), GEOrderBook::from_orders(&[order(50, 1)]));

        let mut gold_budget = 200;
        let purchases = plan_upgrade_purchases(&state, &character, &mut ledger, &mut gold_budget);

        assert_eq!(purchases.len(), 1);
        assert_eq!(purchases[0].code, "cheap_sword");
        assert_eq!(purchases[0].cost, 50);
        assert_eq!(gold_budget, 150);
    }

    // ── batch_capacity_at_current_price ──────────────────────────────────────

    fn craft_info(ingredients: Vec<(&str, i32)>) -> CraftInfo {
        CraftInfo {
            skill: Some("weaponcrafting".into()), level: None,
            items: ingredients.into_iter().map(|(c, q)| CraftIngredient { code: c.into(), quantity: q }).collect(),
            quantity: 1,
        }
    }

    #[test]
    fn batch_capacity_stops_at_the_cheapest_tiers_boundary() {
        let craft = craft_info(vec![("part", 2)]);
        let mut ledger = Ledger::default();
        ledger.market.books.insert("part".into(), GEOrderBook::from_orders(&[order(1, 10), order(5, 90)]));

        // 10 units at the cheapest tier / 2 per craft = 5 crafts before the price would step up.
        assert_eq!(batch_capacity_at_current_price(&craft, &ledger), 5);
    }

    #[test]
    fn batch_capacity_treats_free_bank_stock_as_its_own_flat_region() {
        let craft = craft_info(vec![("part", 1)]);
        let mut ledger = Ledger::default();
        ledger.bank.insert("part".into(), 5);
        ledger.market.books.insert("part".into(), GEOrderBook::from_orders(&[order(1, 1000)]));

        // Free bank stock is cheaper than the paid tier, so crossing from free to paid is itself a
        // price change — the batch stops at 5, not 1005.
        assert_eq!(batch_capacity_at_current_price(&craft, &ledger), 5);
    }

    /// A regression guard for a real bug caught while writing this: `GEOrderBook::claim` leaves a
    /// fully-depleted tier in place at quantity 0 rather than removing it, so naively reading
    /// "the first tier" once it's exhausted would wrongly see 0 room forever after, collapsing the
    /// batch size back down to 1 even though a perfectly good next tier still has stock.
    #[test]
    fn batch_capacity_skips_an_exhausted_leading_tier() {
        let craft = craft_info(vec![("part", 1)]);
        let mut ledger = Ledger::default();
        let mut book = GEOrderBook::from_orders(&[order(1, 5), order(3, 1000)]);
        book.claim(5); // exhaust the cheap tier, leaving it at (1, 0) rather than removed
        ledger.market.books.insert("part".into(), book);

        assert_eq!(batch_capacity_at_current_price(&craft, &ledger), 1000);
    }

    // ── plan_wishlist_purchases ──────────────────────────────────────────────

    /// Crossing a price-tier boundary mid-batch must still price every unit correctly — the cheap
    /// tier's units at their price, the rest at the next tier's price — rather than assuming the
    /// whole batch costs whatever the first tier happened to cost.
    #[test]
    fn plan_wishlist_purchases_batches_correctly_across_a_price_tier_boundary() {
        let items = vec![craft_item("lizard_dagger", "weaponcrafting", None, vec![("lizard_scale", 1)], 1)];
        let data = blank_data(items.clone());
        let state = GameState::new(data);

        let mut character = blank_character();
        character.weaponcrafting_level = 3;
        character.weaponcrafting_xp = 0;
        character.weaponcrafting_max_xp = 100_000;

        let remaining_xp = xp_to_next_tier(character.weaponcrafting_level, character.weaponcrafting_xp, character.weaponcrafting_max_xp) as f64;
        let xp_per_craft = calculate_crafting_xp(character.weaponcrafting_level, &items[0], "weaponcrafting", character.wisdom);
        let crafts_needed = (remaining_xp / xp_per_craft).ceil() as i32;
        assert!(crafts_needed > 5, "test setup needs the tier boundary (at 5 units) to actually be crossed, got {crafts_needed}");

        let mut ledger = Ledger::default();
        // Two price tiers, so satisfying the full tier gap has to cross the boundary between them.
        ledger.market.books.insert("lizard_scale".into(), GEOrderBook::from_orders(&[order(1, 5), order(3, 1_000_000)]));

        let mut gold_budget = 1_000_000_000;
        let purchases = plan_wishlist_purchases(&state, &character, &mut ledger, &mut gold_budget);

        let total_crafts: i32 = purchases.iter().map(|p| p.crafts).sum();
        let total_cost: i32 = purchases.iter().map(|p| p.cost).sum();

        assert!(total_crafts >= crafts_needed, "expected the tier gap to actually be closed");
        let expected_cost = 5 + (total_crafts - 5) * 3;
        assert_eq!(total_cost, expected_cost, "batch must price the cheap tier's 5 units and the rest at the next tier's price, not one flat rate");
    }

    /// The spec's own example: two alternative recipes (here, identical XP/craft by construction)
    /// that could each independently close the same skill's next-tier XP gap must not both be
    /// bought to completion — the combined total should stop around what *one* of them alone would
    /// have needed, not roughly double that.
    #[test]
    fn plan_wishlist_purchases_does_not_double_count_alternative_recipes() {
        let items = vec![
            craft_item("lizard_dagger", "weaponcrafting", None, vec![("lizard_scale", 1)], 1),
            craft_item("mercury_dagger", "weaponcrafting", None, vec![("mercury_bar", 1)], 1),
        ];
        let data = blank_data(items.clone());
        let state = GameState::new(data);

        let mut character = blank_character();
        character.weaponcrafting_level = 3;
        character.weaponcrafting_xp = 0;
        character.weaponcrafting_max_xp = 100_000;

        let remaining_xp = xp_to_next_tier(character.weaponcrafting_level, character.weaponcrafting_xp, character.weaponcrafting_max_xp) as f64;
        let xp_per_craft = calculate_crafting_xp(character.weaponcrafting_level, &items[0], "weaponcrafting", character.wisdom);
        assert_eq!(
            xp_per_craft,
            calculate_crafting_xp(character.weaponcrafting_level, &items[1], "weaponcrafting", character.wisdom),
            "test setup requires identical xp/craft for both recipes to isolate the double-counting check"
        );
        assert!(xp_per_craft > 0.0, "test setup requires a positive xp/craft to be meaningful");

        let mut ledger = Ledger::default();
        ledger.market.books.insert("lizard_scale".into(), GEOrderBook::from_orders(&[order(1, 100_000)]));
        ledger.market.books.insert("mercury_bar".into(), GEOrderBook::from_orders(&[order(1, 100_000)]));

        let mut gold_budget = 1_000_000;
        let purchases = plan_wishlist_purchases(&state, &character, &mut ledger, &mut gold_budget);

        let total_crafts: i32 = purchases.iter()
            .filter(|p| p.code == "lizard_dagger" || p.code == "mercury_dagger")
            .map(|p| p.crafts)
            .sum();
        let max_crafts_for_one_recipe_alone = (remaining_xp / xp_per_craft).ceil() as i32;

        assert!(total_crafts > 0, "expected at least some purchases toward the shared tier gap");
        assert!(
            total_crafts <= max_crafts_for_one_recipe_alone,
            "double-counted the tier gap across alternative recipes: bought {} crafts combined, \
             but only {} were needed to close it once",
            total_crafts, max_crafts_for_one_recipe_alone,
        );
    }

    /// An item on the wishlist purely for its crafting XP must never be bought in its own
    /// finished form, even when that would trivially be the cheapest way to "obtain" it — buying
    /// it grants no crafting XP, which is the entire point of it being on the wishlist. This
    /// deliberately makes the finished item *artificially* cheaper to buy than to craft (1 gold
    /// vs. 5 for the ingredient), so a planner that priced "buy vs. craft" for the wishlist item
    /// itself (as `plan_upgrade_purchases` correctly does for genuine equipment upgrades) would
    /// wrongly take the bait here.
    #[test]
    fn plan_wishlist_purchases_never_buys_the_finished_wishlist_item() {
        let items = vec![craft_item("lizard_dagger", "weaponcrafting", None, vec![("lizard_scale", 1)], 1)];
        let data = blank_data(items.clone());
        let state = GameState::new(data);

        let mut character = blank_character();
        character.weaponcrafting_level = 3;
        character.weaponcrafting_xp = 0;
        character.weaponcrafting_max_xp = 100_000;

        let mut ledger = Ledger::default();
        ledger.market.books.insert("lizard_dagger".into(), GEOrderBook::from_orders(&[order(1, 1_000)])); // tempting bait
        ledger.market.books.insert("lizard_scale".into(), GEOrderBook::from_orders(&[order(5, 1_000)]));

        let mut gold_budget = 1_000_000;
        let purchases = plan_wishlist_purchases(&state, &character, &mut ledger, &mut gold_budget);

        assert!(purchases.iter().any(|p| p.code == "lizard_dagger" && p.crafts > 0), "expected lizard_dagger to actually be crafted");
        assert_eq!(ledger.market.books["lizard_dagger"].claimed_qty, 0, "the finished item itself must never be bought");
        assert!(ledger.market.books["lizard_scale"].claimed_qty > 0, "its ingredient should be bought instead");
    }
}
