// Test fixture: Argument counting for call expressions
// Tests: main -> no_args (0), one_arg (1), two_args (2), three_args (3)

fn no_args() {}

fn one_arg(_a: i32) {}

fn two_args(_a: i32, _b: i32) {}

fn three_args(_a: i32, _b: i32, _c: i32) {}

fn main() {
    no_args();
    one_arg(1);
    two_args(1, 2);
    three_args(1, 2, 3);
}

