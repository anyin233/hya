pub(super) fn workspace(directory: &str) -> String {
    stable_id("wrk", directory)
}

pub(super) fn project(directory: &str) -> String {
    stable_id("proj", directory)
}

fn stable_id(prefix: &str, text: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{prefix}_{hash:016x}")
}
