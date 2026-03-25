// Test fixture: Turbofish syntax in call expressions
// Tests: main -> foo::<u8>, main -> Vec::<i32>::new

fn foo<T>(_value: T) {}

fn main() {
    foo::<u8>(1u8);
    let _ = Vec::<i32>::new();
}

