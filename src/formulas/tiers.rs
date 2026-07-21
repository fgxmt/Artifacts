//! Level tiers/subtiers used to coordinate cross-character role switching (see
//! `loops::promotion`) — a tier is a level band 10 wide (1-9, 10-19, ..., 40-49, 50 on its own),
//! a subtier is a level band 5 wide within a tier (e.g. 10-14 and 15-19 within the 10-19 tier).

/// Tier index for `level`: 1-9 -> 0, 10-19 -> 1, 20-29 -> 2, 30-39 -> 3, 40-49 -> 4, 50 -> 5.
pub fn tier(level: i32) -> i32 {
    level / 10
}

/// True if `level` falls in the lower half of its tier (X0-X4), false if the upper half (X5-X9).
pub fn is_lower_subtier(level: i32) -> bool {
    level % 10 < 5
}

/// Global subtier ordinal — strictly increasing and directly comparable across any two levels
/// regardless of which tier they're in: 1-4 -> 0, 5-9 -> 1, 10-14 -> 2, 15-19 -> 3, 20-24 -> 4,
/// and so on.
pub fn global_subtier(level: i32) -> i32 {
    2 * tier(level) + if is_lower_subtier(level) { 0 } else { 1 }
}

/// The gathering-skill level at which a character should switch from gathering to fighting, given
/// the reference fighting character's `combat_level`: one tier higher than the fighter's own tier
/// if the fighter is in the lower half of that tier, two tiers higher if in the upper half. E.g. a
/// combat level of 13 (tier 10-19, lower half) yields 20; 18 (tier 10-19, upper half) yields 30;
/// 21 (tier 20-29, lower half) also yields 30.
pub fn gather_promotion_threshold(combat_level: i32) -> i32 {
    let bump = if is_lower_subtier(combat_level) { 1 } else { 2 };
    (tier(combat_level) + bump) * 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_promotion_threshold_matches_the_worked_examples() {
        assert_eq!(gather_promotion_threshold(13), 20);
        assert_eq!(gather_promotion_threshold(18), 30);
        assert_eq!(gather_promotion_threshold(21), 30);
    }

    #[test]
    fn tier_and_subtier_boundaries() {
        assert_eq!(tier(1), 0);
        assert_eq!(tier(9), 0);
        assert_eq!(tier(10), 1);
        assert_eq!(tier(19), 1);
        assert_eq!(tier(50), 5);

        assert!(is_lower_subtier(1));
        assert!(is_lower_subtier(4));
        assert!(!is_lower_subtier(5));
        assert!(!is_lower_subtier(9));
        assert!(is_lower_subtier(10));
        assert!(is_lower_subtier(14));
        assert!(!is_lower_subtier(15));
    }

    #[test]
    fn global_subtier_is_strictly_increasing_across_the_given_bands() {
        // "levels 1-4, 5-9, 10-14, 15-19, 20-24, ..."
        let bands = [
            (1, 4), (5, 9), (10, 14), (15, 19), (20, 24), (25, 29),
        ];
        let mut prev_idx = None;
        for (lo, hi) in bands {
            let idx = global_subtier(lo);
            assert_eq!(global_subtier(hi), idx, "band {lo}-{hi} should share one subtier index");
            if let Some(p) = prev_idx {
                assert_eq!(idx, p + 1, "subtier index should increase by exactly 1 per band");
            }
            prev_idx = Some(idx);
        }
    }
}
