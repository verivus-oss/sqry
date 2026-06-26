// Rename twin of `original.rs`: every identifier renamed, every literal changed,
// the control-flow structure preserved exactly. Identifier-blindness means the
// shape_hash and cf_histogram must be byte-identical to `original.rs` (AC-2).
pub fn convert(items: &[i32]) -> i32 {
    let mut accumulator = 1;
    for element in items {
        if *element > 5 {
            accumulator += helper(*element);
        }
    }
    return accumulator;
}
