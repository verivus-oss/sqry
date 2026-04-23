fn imported_only_symbol() {}

const REFERENCED_ONLY_CONST: i32 = 7;

pub(crate) struct UsedViaTypeOf;

impl UsedViaTypeOf {
    fn new() -> Self {
        Self
    }
}

pub(crate) fn drive_imports() {
    use crate::reachability::imported_only_symbol as imported_alias;

    let _ = imported_alias as fn();
}

pub(crate) fn drive_references() -> i32 {
    REFERENCED_ONLY_CONST
}

pub(crate) fn drive_type_of() -> UsedViaTypeOf {
    UsedViaTypeOf::new()
}

fn reach_cycle_left() {
    reach_cycle_right();
}

fn reach_cycle_right() {
    reach_cycle_left();
}
