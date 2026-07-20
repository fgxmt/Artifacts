use crate::api::GameData;
use crate::types::Character;

use super::combat_sim::best_achievable_xph;
use super::eligibility::{adjust_combat_stats, meets_conditions};
use super::slot_rating::{current_item, rate_gathering_item, slot_definitions, total_gathering_cooldown_pct, ItemRole, RankedItem, SlotRating};

// ── Joint (brute-force) loadout optimization ────────────────────────────────
//
// optimize_items rates each slot's items independently, holding every other slot at its
// currently-equipped item. That marginal analysis can miss cases where a monster only becomes
// winnable through the *combined* effect of several simultaneously-equipped items, none of which
// individually crosses the survivability threshold. A truly exhaustive search over every slot
// combination isn't feasible — a level-45 character's real item pool multiplies out to ~10^14
// combinations across the 13 slots even before considering every equippable item in the game (not
// just ones near the character's level). Instead, this runs several randomized-order
// coordinate-ascent searches in parallel (native OS threads —
// this is CPU-bound, not I/O): each repeatedly re-optimizes one slot at a time (trying every
// candidate for that slot, exhaustively, against the current best guess for every other slot)
// and cycles through slots until a full pass changes nothing. Because every stat an item can
// grant (attack/dmg/crit/haste/initiative/wisdom, hp/resistances) is monotonically non-decreasing
// in its effect on best-achievable XP/hour, this reliably converges to the joint optimum in
// practice, and — since every search seeds from the character's *current* loadout — can never do
// worse than leaving things as they are.

// One restart always seeds from the character's actual current loadout (guaranteeing the result
// is never worse than doing nothing); the rest seed from randomized per-slot picks so the search
// has a real chance of landing near — and then hill-climbing to lock in — a combination where no
// single slot's change alone improves things (see `brute_force_slots`).
const BRUTE_FORCE_RESTARTS: usize = 1024;
const BRUTE_FORCE_MAX_PASSES: usize = 25;

struct SlotCandidates {
    slot: &'static str,
    current_code: Option<String>,
    /// Every candidate worth trying in this slot: `None` (empty) plus the currently-equipped item
    /// plus every item of the matching category whose conditions the character meets. No level
    /// window — an item's own `conditions` (every equippable item carries a `level gt N` entry)
    /// already excludes anything the character isn't high-enough level to equip, so the only
    /// effect of not windowing is also considering items the character has *outgrown*, which is
    /// exactly what's needed to correctly rank a low-level filler item against not filling the
    /// slot at all.
    candidates: Vec<Option<String>>,
}

fn combat_candidate_pool(data: &GameData, character: &Character, category: &'static str, current_code: &str) -> Vec<Option<String>> {
    let mut codes: Vec<Option<String>> = vec![None];
    if !current_code.is_empty() {
        codes.push(Some(current_code.to_string()));
    }
    for item in data.items.iter().filter(|i| i.item_type == category) {
        if meets_conditions(character, item) {
            let already_listed = codes.iter().any(|c| c.as_deref() == Some(item.code.as_str()));
            if !already_listed {
                codes.push(Some(item.code.clone()));
            }
        }
    }
    codes
}

/// Best achievable XP/hour for `naked` (a character with every eligible slot's contribution
/// already stripped out) with `chosen[i]` equipped into `slots[i]` for each slot.
fn score_loadout(naked: &Character, slots: &[SlotCandidates], chosen: &[Option<String>], data: &GameData) -> f64 {
    let mut working = naked.clone();
    for code in chosen.iter().flatten() {
        if let Some(item) = data.items.iter().find(|i| &i.code == code) {
            working = adjust_combat_stats(&working, item, 1);
        }
    }
    let _ = slots;
    best_achievable_xph(&working, data)
}

/// Repeatedly sweeps `order` re-optimizing one slot at a time (holding every other slot at its
/// current best) until a full sweep changes nothing, or `BRUTE_FORCE_MAX_PASSES` is hit.
fn coordinate_ascent_pass(
    naked: &Character,
    slots: &[SlotCandidates],
    data: &GameData,
    mut chosen: Vec<Option<String>>,
    order: &[usize],
) -> (Vec<Option<String>>, f64) {
    let mut current_score = score_loadout(naked, slots, &chosen, data);

    for _pass in 0..BRUTE_FORCE_MAX_PASSES {
        let mut changed = false;

        for &i in order {
            let mut best_choice = chosen[i].clone();
            let mut best_score  = current_score;

            for cand in &slots[i].candidates {
                if *cand == chosen[i] { continue; }
                let mut trial = chosen.clone();
                trial[i] = cand.clone();
                let score = score_loadout(naked, slots, &trial, data);
                // A candidate that exactly ties the current best is still preferred over leaving
                // the slot empty: every stat gear can grant is never negative, so a tie usually
                // just means this particular item's bonus doesn't happen to move best-achievable
                // XP/hour (e.g. pure resistance that doesn't unlock a new monster or speed up the
                // current one) — not that wearing it is pointless. Ties between two *different*
                // non-empty candidates aren't worth resolving the same way (nothing distinguishes
                // them under the model), so this only ever moves off of empty.
                let breaks_tie_from_empty = score == best_score && cand.is_some() && best_choice.is_none();
                if score > best_score || breaks_tie_from_empty {
                    best_score  = score;
                    best_choice = cand.clone();
                }
            }

            if best_choice != chosen[i] {
                chosen[i]     = best_choice;
                current_score = best_score;
                changed = true;
            }
        }

        if !changed { break; }
    }

    (chosen, current_score)
}

/// Cheap deterministic xorshift64 step — no `rand` dependency in this codebase, and restarts only
/// need *different* pseudo-random sequences from each other, not true randomness.
fn xorshift_next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn shuffled_order(len: usize, rng_state: &mut u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    for i in (1..order.len()).rev() {
        let j = (xorshift_next(rng_state) as usize) % (i + 1);
        order.swap(i, j);
    }
    order
}

/// Character with every `slots` entry's currently-equipped item contribution stripped out — the
/// baseline both the search and its final ranking build back up from.
fn naked_for_slots(character: &Character, data: &GameData, slots: &[SlotCandidates]) -> Character {
    let mut naked = character.clone();
    for sc in slots {
        if let Some(item) = current_item(data, sc.current_code.as_deref().unwrap_or("")) {
            naked = adjust_combat_stats(&naked, item, -1);
        }
    }
    naked
}

/// Runs `BRUTE_FORCE_RESTARTS` coordinate-ascent searches in parallel threads, each exploring
/// slots in a different (cheaply pseudo-randomized) order so restarts don't all get stuck in the
/// same local optimum, and keeps the best-scoring result. Every search starts from the
/// character's current loadout for `slots`, so the result is guaranteed at least as good.
fn brute_force_slots(character: &Character, data: &GameData, slots: &[SlotCandidates]) -> (Vec<Option<String>>, f64) {
    let naked = naked_for_slots(character, data, slots);

    let current: Vec<Option<String>> = slots.iter().map(|sc| sc.current_code.clone()).collect();

    let results: Vec<(Vec<Option<String>>, f64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..BRUTE_FORCE_RESTARTS)
            .map(|seed| {
                let naked   = &naked;
                let current = current.clone();
                scope.spawn(move || {
                    let mut rng_state: u64 = 0x9E3779B97F4A7C15u64.wrapping_mul(seed as u64 + 1);
                    let order = shuffled_order(slots.len(), &mut rng_state);

                    // Restart 0 always seeds from the character's actual current loadout, so the
                    // overall best-of-restarts result can never be worse than doing nothing. The
                    // rest seed from a randomized per-slot pick — coordinate ascent only ever
                    // moves one slot at a time, so a search that always starts from "everything
                    // empty/current" can get stuck exactly when an upgrade only pays off once
                    // *several* slots change together (no single slot's change alone improves
                    // things, so a same-starting-point hill climb never takes the first step).
                    // Landing near such a combination by chance in a randomized start lets these
                    // restarts discover and then lock in that plateau.
                    let starting = if seed == 0 {
                        current
                    } else {
                        slots.iter()
                            .map(|sc| sc.candidates[(xorshift_next(&mut rng_state) as usize) % sc.candidates.len()].clone())
                            .collect()
                    };

                    coordinate_ascent_pass(naked, slots, data, starting, &order)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    results
        .into_iter()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap()
}

/// For every touched slot, ranks every one of its candidates by the XP/hour delta it would add
/// *given every other touched slot fixed at `chosen`* — i.e. the same joint context the search
/// converged on, not the character's original bare stats. This is what makes the ranking usable
/// for availability fallback: item rank 6 in the returned list is genuinely "6th best assuming
/// you're already wearing the other brute-forced picks", so equipping it later (if ranks 1-5
/// turn out to be unobtainable) is still a sound choice rather than a stale one-off comparison.
fn rank_touched_slots(
    naked: &Character,
    slots: &[SlotCandidates],
    chosen: &[Option<String>],
    data: &GameData,
) -> Vec<Vec<RankedItem>> {
    (0..slots.len())
        .map(|i| {
            let mut baseline_chosen = chosen.to_vec();
            baseline_chosen[i] = None;
            let baseline = score_loadout(naked, slots, &baseline_chosen, data);

            let mut ranked: Vec<RankedItem> = slots[i].candidates.iter()
                .filter_map(|cand| {
                    let code = cand.as_ref()?;
                    let mut trial = chosen.to_vec();
                    trial[i] = Some(code.clone());
                    let rating = score_loadout(naked, slots, &trial, data) - baseline;
                    // >= 0, not > 0: an item can easily tie *exactly* with empty here (its real
                    // stats are too small to shift best-achievable XP/hour once the rest of the
                    // loadout is already strong, or its XP contribution rounds away) while still
                    // being a genuine upgrade over actually wearing nothing. Dropping those would
                    // lose them as fallback candidates entirely — e.g. plain Copper Boots (+10 hp
                    // / +10 wisdom, no level requirement) tying with empty next to a much stronger
                    // Iron Boots pick, but still worth grabbing if Iron Boots isn't obtainable.
                    // Every stat an item can grant is monotonically non-negative, so nothing here
                    // should ever come out truly negative other than a genuinely cursed item.
                    (rating >= 0.0).then(|| RankedItem { code: code.clone(), rating })
                })
                .collect();
            ranked.sort_by(|a, b| b.rating.partial_cmp(&a.rating).unwrap_or(std::cmp::Ordering::Equal));
            ranked
        })
        .collect()
}

/// Joint combinatorial re-optimization pass, meant to run once right after `optimize_items` for
/// the same character/role. For the fight-loop character (`ItemRole::Combat`) this jointly
/// re-searches all 13 slots. For gathering characters, slots the gathering formula actually
/// affects (wisdom/cooldown effects) are already globally optimal from independent per-slot
/// analysis — there's no threshold-crossing effect to miss there, since gathering XP/hour is a
/// smooth, monotonic function of those stats — so only the "combat fallback" slots (the ones
/// `optimize_items` couldn't find any gathering-relevant item for) are re-searched, jointly,
/// against a combat objective. Candidates are every item of the matching category whose
/// conditions the character currently meets (see `combat_candidate_pool`) — coordinate ascent
/// explores one slot at a time rather than the full cross product, so this stays tractable without
/// needing a level window; see the module-level comment above for why the *cross product* isn't
/// feasible. Returns `current_ratings` with the brute-forced slots' `ranked` list replaced by the joint
/// ranking (see `rank_touched_slots`) — not just the single winning combination.
pub fn brute_force_optimal_loadout(
    name: &str,
    character: &Character,
    role: &ItemRole<'_>,
    data: &GameData,
    current_ratings: &[SlotRating],
) -> Vec<SlotRating> {
    let slot_defs = slot_definitions(character);

    let eligible_slots: Vec<&'static str> = match role {
        ItemRole::Combat => slot_defs.iter().map(|(s, _, _)| *s).collect(),
        ItemRole::Gathering { skill, resource } => {
            let total_cd = total_gathering_cooldown_pct(character, data, skill);
            slot_defs.iter()
                .filter(|(_, category, current_code)| {
                    let current = current_item(data, current_code);
                    !data.items.iter()
                        .filter(|i| &i.item_type == category)
                        .any(|i| rate_gathering_item(character, resource, skill, total_cd, current, i, data) > 0.0)
                })
                .map(|(s, _, _)| *s)
                .collect()
        }
    };

    if eligible_slots.is_empty() {
        // Nothing left for the joint search to do — the loadout coming in is already complete,
        // so this is still the first point at which it's safe to print it.
        print_final_loadout(name, current_ratings, data);
        return current_ratings.to_vec();
    }

    let slots: Vec<SlotCandidates> = slot_defs.iter()
        .filter(|(s, _, _)| eligible_slots.contains(s))
        .map(|(slot, category, current_code)| SlotCandidates {
            slot,
            current_code: if current_code.is_empty() { None } else { Some(current_code.to_string()) },
            candidates: combat_candidate_pool(data, character, category, current_code),
        })
        .collect();

    println!(
        "[{}] Brute-force loadout search ({} slot(s), {} parallel restarts)...",
        crate::ts_char(name), slots.len(), BRUTE_FORCE_RESTARTS
    );

    let (chosen, score) = brute_force_slots(character, data, &slots);

    println!(
        "[{}] Brute-force result: {:.0} best-achievable XP/hr (joint search over {} slot(s))",
        crate::ts_char(name), score, slots.len()
    );

    let naked = naked_for_slots(character, data, &slots);
    let ranked_per_slot = rank_touched_slots(&naked, &slots, &chosen, data);

    let mut updated = current_ratings.to_vec();
    for ((sc, _choice), ranked) in slots.iter().zip(chosen.iter()).zip(ranked_per_slot) {
        if let Some(r) = updated.iter_mut().find(|r| r.slot == sc.slot) {
            r.ranked = ranked;
        }
    }

    // The complete loadout — primary-role slots the joint search never touched plus every
    // brute-forced slot — printed exactly once, now that it's actually settled.
    print_final_loadout(name, &updated, data);

    updated
}

/// Prints the complete, final per-slot loadout (top pick, its rating, and how many ranked
/// alternatives exist below it) — always called after every slot has reached its final value, so
/// this never shows a partial/pre-brute-force snapshot.
fn print_final_loadout(name: &str, ratings: &[SlotRating], data: &GameData) {
    println!("[{}] Final item loadout:", crate::ts_char(name));
    for r in ratings {
        let display = match r.ranked.first() {
            Some(top) => data.items.iter().find(|i| i.code == top.code)
                .map(|i| i.name.clone())
                .unwrap_or_else(|| top.code.clone()),
            None => "(empty)".to_string(),
        };
        println!(
            "[{}]   {:<12} {:<40} rating {:+.2} ({} alternative(s) ranked below)",
            crate::ts_char(name), r.slot, display,
            r.ranked.first().map(|x| x.rating).unwrap_or(0.0),
            r.ranked.len().saturating_sub(1),
        );
    }
}
