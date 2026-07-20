use std::sync::Arc;
use std::time::Duration;

use artifactsmmo::{
    build_client,
    flags::GameState,
    get_bank_items, load_game_data,
    loops::{alchemy_and_crafting_loop, fight_loop, gather_loop, init_character_ratings, print_initial_crafting_plan},
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

    let mut character4: Option<Character> = None;
    for (name, role_skill) in roles {
        match init_character_ratings(&client, name, role_skill, &state).await {
            Ok(character) => {
                println!("[init] Ratings cached for {}", name);
                if name == CHARACTER4 {
                    character4 = Some(character);
                }
            }
            Err(e) => eprintln!("[init] Failed to fetch {} for initial ratings: {}", name, e),
        }
    }

    match character4 {
        Some(character) => print_initial_crafting_plan(&state, &character).await,
        None => eprintln!("[init] Skipping initial crafting plan preview (failed to fetch {})", CHARACTER4),
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
