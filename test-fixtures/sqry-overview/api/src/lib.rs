//! API layer that drives the core engine.

/// Duplicate body #1 (identical to handle_two).
pub fn handle_one(a: u32, b: u32) -> u32 {
    let total = a.wrapping_add(b);
    let scaled = total.wrapping_mul(11);
    let mixed = scaled ^ 0x1234;
    mixed.rotate_left(3)
}

/// Duplicate body #2 (identical to handle_one).
pub fn handle_two(a: u32, b: u32) -> u32 {
    let total = a.wrapping_add(b);
    let scaled = total.wrapping_mul(11);
    let mixed = scaled ^ 0x1234;
    mixed.rotate_left(3)
}

/// Drives the core engine (cross-subsystem coupling: api -> core).
pub fn dispatch(x: u32) -> u32 {
    let normalized = engine_run(x);
    let checked = validate(normalized);
    handle_one(checked, 1)
}
