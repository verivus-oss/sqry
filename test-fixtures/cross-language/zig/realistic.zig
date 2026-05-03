// SPDX-License-Identifier: MIT
const SessionState = struct {
    mutableField: []const u8,
    immutableField: []const u8,
    sharedName: usize,
    pub const staticField: usize = 1;
};

const AuditState = struct {
    sharedName: usize,
};

pub fn readSession(state: SessionState) usize {
    return state.sharedName + SessionState.staticField;
}
