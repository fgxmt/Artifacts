use artifactsmmo::{build_client, load_game_data};

#[tokio::main]
async fn main() -> artifactsmmo::Result<()> {
    let client = build_client();
    let data = load_game_data(&client).await?;

    std::fs::write("monsters.json",        serde_json::to_string_pretty(&data.monsters)?)?;
    std::fs::write("items.json",           serde_json::to_string_pretty(&data.items)?)?;
    std::fs::write("resources.json",       serde_json::to_string_pretty(&data.resources)?)?;
    std::fs::write("maps.json",            serde_json::to_string_pretty(&data.maps)?)?;
    std::fs::write("craftable_equip.json", serde_json::to_string_pretty(&data.craftable_equip)?)?;

    println!("monsters.json       — {} entries", data.monsters.len());
    println!("items.json          — {} entries", data.items.len());
    println!("resources.json      — {} entries", data.resources.len());
    println!("maps.json           — {} entries", data.maps.len());
    println!("craftable_equip.json — {} entries", data.craftable_equip.len());

    Ok(())
}
