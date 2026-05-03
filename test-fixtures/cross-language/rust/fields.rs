pub struct Ledger {
    pub mutable_field: i32,
    immutable_field: i32,
    pub shared_name: i32,
}

pub struct Archive {
    pub shared_name: i32,
}

pub enum FieldEvent {
    Changed { mutable_field: i32 },
    Indexed(i32),
}

pub fn read_ledger(ledger: &Ledger) -> i32 {
    ledger.mutable_field + ledger.shared_name
}
