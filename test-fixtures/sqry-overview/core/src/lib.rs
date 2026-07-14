//! Core engine module.

/// Load-bearing entry: everyone depends on this.
pub fn engine_run(input: u32) -> u32 {
    let a = normalize(input);
    let b = validate(a);
    transform(b)
}

pub fn normalize(x: u32) -> u32 {
    x.saturating_add(1)
}

pub fn validate(x: u32) -> u32 {
    if x > 0 { x } else { normalize(x) }
}

pub fn transform(x: u32) -> u32 {
    let step_one = x.wrapping_mul(3);
    let step_two = step_one.wrapping_add(7);
    let step_three = step_two ^ 0x5a5a;
    step_three.rotate_left(2)
}

/// Mutually recursive with helper_b to plant a cycle.
pub fn helper_a(x: u32) -> u32 {
    helper_b(x.saturating_sub(1))
}

pub fn helper_b(x: u32) -> u32 {
    if x == 0 { 0 } else { helper_a(x) }
}

/// A public API with no internal callers.
pub fn dead_public_api(x: u32) -> u32 {
    x.wrapping_mul(2)
}
