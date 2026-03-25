// Test fixture: Nested function scopes
// Tests: outer::inner, outer::helper

fn outer(x: i32) -> i32 {
    fn inner(y: i32) -> i32 {
        y * 2
    }

    fn helper(z: i32) -> i32 {
        z + 10
    }

    let step1 = inner(x);
    let step2 = helper(step1);
    step2
}

fn process_with_nested(value: i32) -> i32 {
    fn validate(v: i32) -> bool {
        v > 0
    }

    fn transform(v: i32) -> i32 {
        fn double(n: i32) -> i32 {
            n * 2
        }

        fn add_one(n: i32) -> i32 {
            n + 1
        }

        let doubled = double(v);
        add_one(doubled)
    }

    if validate(value) {
        transform(value)
    } else {
        0
    }
}

fn with_closure_and_nested() -> i32 {
    fn inner_fn(x: i32) -> i32 {
        x * 3
    }

    let closure = |y: i32| inner_fn(y) + 1;

    closure(10)
}

fn main() {
    let result1 = outer(5);
    println!("Outer result: {}", result1);

    let result2 = process_with_nested(7);
    println!("Process result: {}", result2);

    let result3 = with_closure_and_nested();
    println!("Closure+nested result: {}", result3);
}
