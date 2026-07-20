use artifactsmmo::{
    build_client, find_optimal_crafting, find_optimal_gathering,
    find_optimal_monster, load_game_data,
    secrets::CHARACTER0,
};

#[tokio::main]
async fn main() -> artifactsmmo::Result<()> {
    let client = build_client();
    let name   = CHARACTER0;
    let data   = load_game_data(&client).await?;

    // Combat
    find_optimal_monster(&client, name, &data).await?;

    println!();

    // Gathering-only: Fishing
    find_optimal_gathering(&client, name, "woodcutting", &data).await?;
    find_optimal_gathering(&client, name, "mining", &data).await?;
    find_optimal_gathering(&client, name, "alchemy", &data).await?;
    find_optimal_gathering(&client, name, "fishing", &data).await?;

    println!();

    // Crafting-only skills
    for skill in &["weaponcrafting", "gearcrafting", "jewelrycrafting", "cooking"] {
        find_optimal_crafting(&client, name, skill, &data).await?;
        println!();
    }

    Ok(())
}
