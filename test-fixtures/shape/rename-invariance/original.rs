// AC-2/AC-3 anchor (Rust). `transform` carries branch + loop + call + return +
// assign control flow. `renamed.rs` is the same structure with every identifier
// and literal changed; `reformatted.rs` is this body with comments and
// whitespace added. All three must produce one identical shape_hash.
pub fn transform(values: &[i32]) -> i32 {
    let mut total = 0;
    for value in values {
        if *value > 0 {
            total += scale(*value);
        }
    }
    return total;
}
