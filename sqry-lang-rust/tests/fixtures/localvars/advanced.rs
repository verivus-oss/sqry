// Advanced: closures, match, if-let, destructuring

fn closure_capture() {
    let x = 10;
    let f = |y| x + y;
    println!("{}", f(1));
}

fn match_binding() {
    let value = Some(42);
    match value {
        Some(inner) => {
            let _result = inner + 1;
        }
        None => {}
    }
}

fn if_let_binding() {
    let value = Some(42);
    if let Some(inner) = value {
        let _result = inner + 1;
    }
}

fn destructuring_tuple() {
    let pair = (1, 2);
    let (a, b) = pair;
    println!("{} {}", a, b);
}

fn destructuring_struct() {
    struct Point { x: i32, y: i32 }
    let p = Point { x: 10, y: 20 };
    let Point { x, y } = p;
    println!("{} {}", x, y);
}

fn shadowing_idiomatic() {
    let x = "hello";
    let x = x.len();
    println!("{}", x);
}

fn unsafe_block_var() {
    unsafe {
        let raw = 42;
        println!("{}", raw);
    }
}
