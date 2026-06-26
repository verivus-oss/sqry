// Reformat twin of `original.rs`: identical identifiers and literals, but with
// comments injected and whitespace reflowed inside the body. tree-sitter treats
// comments and whitespace as extras, so the walker skips them: the descriptor
// (cf_histogram + shape_hash) is unchanged while body_hash, computed from the
// raw body bytes, DIFFERS (AC-3).
pub fn transform(values: &[i32]) -> i32 {
    // running total of the scaled positive values
    let mut total = 0;

    for value in values {
        // only positive contributions are scaled
        if *value > 0 {
            total += scale(*value); // accumulate
        }
    }

    return total; // done
}
