use artifactsmmo::secrets::CHARACTER0;
use artifactsmmo::{build_client, move_character};

#[tokio::main]
async fn main() {
    let client = build_client();

    if let Err(e) = move_character(&client, CHARACTER0, 4, 1).await {
        eprintln!("Error: {}", e);
    }
}
