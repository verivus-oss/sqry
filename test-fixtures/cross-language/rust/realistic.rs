// SPDX-License-Identifier: MIT
pub struct SessionState<T> {
    pub mutable_field: T,
    pub shared_name: usize,
    private_field: bool,
}

pub struct AuditState {
    pub shared_name: usize,
}

impl<T> SessionState<T> {
    pub fn new(value: T) -> Self {
        Self {
            mutable_field: value,
            shared_name: 0,
            private_field: false,
        }
    }
}
