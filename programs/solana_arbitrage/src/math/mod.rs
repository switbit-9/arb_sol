pub fn safe_div(numerator: &u128, denominator: &u128) -> u128 {
    if *denominator == 0 {
        return 0;
    }
    // Scale numerator by 1e9 for fixed point precision
    numerator
        .checked_mul(1_000_000_000)
        .and_then(|n| n.checked_div(*denominator))
        .unwrap_or(0)
}
