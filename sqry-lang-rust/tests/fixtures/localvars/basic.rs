// Basic variable declaration + usage tests

fn simple_var() {
    let x = 10;
    let y = x + 1;
    println!("{}", y);
}

fn const_binding() {
    let count = 42;
    println!("{}", count);
}

fn mutable_var() {
    let mut x = 10;
    x += 1;
    println!("{}", x);
}

fn param_ref(name: &str, age: u32) {
    let result = name;
    println!("{} {}", result, age);
}
