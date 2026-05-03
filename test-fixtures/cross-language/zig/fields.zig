const Ledger = struct {
    mutableField: i32,
    immutableField: i32,
    sharedName: i32,
    pub const staticField: i32 = 1;
};

const Archive = struct {
    sharedName: i32,
};

pub fn readLedger(ledger: Ledger) i32 {
    return ledger.mutableField + Ledger.staticField;
}
