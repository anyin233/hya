//! Golden-diff helpers for parity gates.

use crate::capture::normalize;

/// Compare two frames after normalization. Returns `Ok(())` if equal, else a unified-ish diff.
///
/// # Errors
/// Returns the line-by-line difference when frames differ after normalization.
pub fn assert_frames_equal(actual: &str, expected: &str) -> Result<(), String> {
    let a = normalize(actual);
    let e = normalize(expected);
    if a == e {
        return Ok(());
    }
    let mut out = String::from("frames differ:\n");
    let al: Vec<&str> = a.lines().collect();
    let el: Vec<&str> = e.lines().collect();
    for i in 0..al.len().max(el.len()) {
        let got = al.get(i).copied().unwrap_or("<none>");
        let want = el.get(i).copied().unwrap_or("<none>");
        if got != want {
            out.push_str(&format!("  line {i}: got {got:?} want {want:?}\n"));
        }
    }
    Err(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_after_normalization() {
        assert!(assert_frames_equal("a \nb\n\n", "a\nb").is_ok());
    }

    #[test]
    fn reports_difference() {
        assert!(assert_frames_equal("a\nx", "a\nb").is_err());
    }
}
