use std::sync::Arc;
use std::time::Duration;

use artifactsmmo::{
    build_client,
    flags::GameState,
    get_bank_details, get_bank_items, load_game_data,
    loops::{
        alchemy_and_crafting_loop, fight_loop, gather_loop, init_character_ratings,
        plan_ge_purchases, print_ge_purchase_plan, print_initial_crafting_plan, seed_shared_state,
    },
    secrets::{CHARACTER0, CHARACTER1, CHARACTER2, CHARACTER3, CHARACTER4},
    Character,
};

#[tokio::main]
async fn main() {
    let client = build_client();

    println!("[init] Loading game data...");
    let data = match load_game_data(&client).await {
        Ok(data) => data,
        Err(e) => {
            eprintln!("[init] Failed to load game data: {}", e);
            return;
        }
    };
    println!(
        "[init] Loaded {} monsters, {} items, {} resources, {} maps, {} craftable equipment",
        data.monsters.len(), data.items.len(), data.resources.len(), data.maps.len(), data.craftable_equip.len(),
    );

    let state = GameState::new(data);

    println!("[init] Loading bank contents...");
    match get_bank_items(&client).await {
        Ok(bank) => {
            let total: i32 = bank.iter().map(|b| b.quantity).sum();
            println!("[init] Bank loaded: {} stacks ({} total items)", bank.len(), total);
            state.update_bank(bank).await;
        }
        Err(e) => eprintln!("[init] Failed to load bank contents: {}", e),
    }

    // Sequentially (not in parallel, to stay well under the rate limit) fetch each character and
    // cache their item ratings against their optimal target, so the crafting plan below reflects
    // real equip-upgrade demand from the very first run instead of starting empty.
    println!("[init] Computing initial item ratings for each character...");
    let roles: [(&'static str, Option<&'static str>); 5] = [
        (CHARACTER0, None),
        (CHARACTER1, Some("woodcutting")),
        (CHARACTER2, Some("mining")),
        (CHARACTER3, Some("fishing")),
        (CHARACTER4, Some("alchemy")),
    ];

    let mut character0: Option<Character> = None;
    let mut character4: Option<Character> = None;
    for (name, role_skill) in roles {
        match init_character_ratings(&client, name, role_skill, &state).await {
            Ok(character) => {
                println!("[init] Ratings cached for {}", name);
                if name == CHARACTER0 {
                    character0 = Some(character);
                } else if name == CHARACTER4 {
                    character4 = Some(character);
                }
            }
            Err(e) => eprintln!("[init] Failed to fetch {} for initial ratings: {}", name, e),
        }
    }

    // Seed the cross-character promotion/drop-farming reference points (see loops::gather,
    // loops::alchemy, loops::repositioning) before any loop starts, so mining/woodcutting/fishing/
    // alchemy characters don't briefly compare against a default fighter level of 1, and fighting
    // characters don't briefly think the crafter's min gear level is 1 (which every fighting
    // character would trivially exceed, wrongly switching to drop-farming immediately).
    match (&character0, &character4) {
        (Some(fighter), Some(crafter)) => seed_shared_state(&state, fighter, crafter),
        _ => eprintln!(
            "[init] Skipping promotion/drop-farming state seeding (failed to fetch {} and/or {})",
            CHARACTER0, CHARACTER4
        ),
    }

    match &character4 {
        Some(character) => print_initial_crafting_plan(&state, character).await,
        None => eprintln!("[init] Skipping initial crafting plan preview (failed to fetch {})", CHARACTER4),
    }

    // Grand Exchange purchase planning — calculation only for now (see loops::grand_exchange's
    // module doc comment); nothing here actually posts a buy order. Available gold is the
    // crafting character's own held gold plus the bank's shared gold balance, since either can
    // fund a purchase once buying is actually wired up.
    match &character4 {
        Some(character) => {
            let bank_gold = match get_bank_details(&client).await {
                Ok(details) => details.gold,
                Err(e) => {
                    eprintln!("[init] Failed to fetch bank gold balance (assuming 0): {}", e);
                    0
                }
            };
            let available_gold = character.gold + bank_gold;
            let plan = plan_ge_purchases(&client, &state, character, available_gold).await;
            print_ge_purchase_plan(CHARACTER4, &plan);
        }
        None => eprintln!("[init] Skipping Grand Exchange purchase plan (failed to fetch {})", CHARACTER4),
    }

    tokio::time::sleep(Duration::from_secs(1)).await;

    let handles = [
        tokio::spawn(fight_loop(CHARACTER0, Arc::clone(&state))),
        tokio::spawn(gather_loop(CHARACTER1, "woodcutting", Arc::clone(&state))),
        tokio::spawn(gather_loop(CHARACTER2, "mining", Arc::clone(&state))),
        tokio::spawn(gather_loop(CHARACTER3, "fishing", Arc::clone(&state))),
        tokio::spawn(alchemy_and_crafting_loop(CHARACTER4, Arc::clone(&state))),
    ];

    for handle in handles {
        let _ = handle.await;
    }
}
