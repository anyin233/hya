//! Backend spike (PLAN.md W1): prove `hya` can spawn and fully talk to a real
//! backend server — parse the URL, hit `/config`, and stream `/global/event`.
//!
//! Run: `cargo run -p hya_sdk --example backend_spike -- --cwd /path/to/dir`

use std::time::Duration;

use hya_sdk::{stream_global_events, Client, HttpClient, ServerHandle};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = parse_cwd().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string()
    });

    let server = ServerHandle::spawn(&dir).await?;
    println!("LISTENING {}", server.base_url());

    let http = reqwest::Client::new();
    let client = HttpClient::with_http(http.clone(), server.base_url(), &dir);
    client.config_get().await?;
    println!("CONFIG OK");

    let base = server.base_url().to_string();
    let dir_for_activity = dir.clone();
    let http_for_activity = http.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let activity = HttpClient::with_http(http_for_activity, base, dir_for_activity);
        let _ = activity.session_create().await;
    });

    let mut count = 0usize;
    let stream = stream_global_events(&http, server.base_url(), &dir, |event| {
        println!("EVENT {}", event.payload.kind);
        count += 1;
        count < 8
    });
    let _ = tokio::time::timeout(Duration::from_secs(12), stream).await;
    println!("TOTAL_EVENTS {count}");

    Ok(())
}

fn parse_cwd() -> Option<String> {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == "--cwd" {
            return args.next();
        }
    }
    None
}
