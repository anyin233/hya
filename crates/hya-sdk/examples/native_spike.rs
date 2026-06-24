use std::path::PathBuf;
use std::time::Duration;

use hya_sdk::{Client, NativeBridge};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pkg = std::env::var("HYA_BACKEND_DIR")
        .map(PathBuf::from)
        .expect("set HYA_BACKEND_DIR to the backend package dir");
    let workdir = std::env::current_dir()?.display().to_string();

    println!("SPAWNING bridge from {}", pkg.display());
    let bridge = NativeBridge::spawn(&pkg, mpsc_sink()).await?;
    println!("READY");

    let client = bridge.client(&workdir);
    let config = client.config_get().await?;
    println!("CONFIG theme={:?}", config.theme);
    let agents = client.agents().await?;
    println!("AGENTS count={}", agents.len());
    let sessions = client.session_list().await?;
    println!("SESSIONS count={}", sessions.len());
    println!("OK");
    Ok(())
}

fn mpsc_sink() -> mpsc::UnboundedSender<hya_sdk::GlobalEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel::<hya_sdk::GlobalEvent>();
    tokio::spawn(async move {
        let mut count = 0u32;
        while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            count += 1;
            if count <= 5 {
                println!("EVENT {}", event.payload.kind);
            }
        }
        println!("EVENTS total~{count}");
    });
    tx
}
