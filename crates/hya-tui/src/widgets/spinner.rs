use std::time::Duration;

pub const INTERVAL: Duration = Duration::from_millis(80);
pub const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const FRAME_COUNT: u128 = 10;

#[must_use]
pub fn frame(elapsed: Duration) -> &'static str {
    let interval_ms = INTERVAL.as_millis().max(1);
    match (elapsed.as_millis() / interval_ms) % FRAME_COUNT {
        0 => FRAMES[0],
        1 => FRAMES[1],
        2 => FRAMES[2],
        3 => FRAMES[3],
        4 => FRAMES[4],
        5 => FRAMES[5],
        6 => FRAMES[6],
        7 => FRAMES[7],
        8 => FRAMES[8],
        9 => FRAMES[9],
        _ => FRAMES[0],
    }
}

#[must_use]
pub fn ellipsis_fallback(message: &str) -> String {
    if message.is_empty() {
        return "⋯".to_owned();
    }
    format!("⋯ {message}")
}
