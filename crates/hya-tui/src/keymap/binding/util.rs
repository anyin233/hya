pub(super) fn starts_with_ignore_ascii_case(input: &str, prefix: &str) -> bool {
    input
        .get(..prefix.len())
        .is_some_and(|actual| actual.eq_ignore_ascii_case(prefix))
}
