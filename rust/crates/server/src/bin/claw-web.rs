use std::net::SocketAddr;

use server::{app, AppState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::var("CLAW_WEB_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(4317);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("Claw web is running at http://{address}");
    axum::serve(listener, app(AppState::new())).await?;
    Ok(())
}
