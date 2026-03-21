pub fn process(x: i32) -> i32 {
    helper(x) * 2
}

fn helper(x: i32) -> i32 {
    x + 1
}
