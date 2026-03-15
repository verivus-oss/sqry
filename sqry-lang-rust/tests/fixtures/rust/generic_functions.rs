// Test fixture: Generic function definitions and calls
// Tests: main -> generic, generic -> helper

fn helper<T>(_value: &T) {}

fn generic<T>(value: T) {
    helper(&value);
}

fn main() {
    generic(1u8);
}

