// Scoping and loop variable tests

fn shadowed_var() {
    let x = 1;
    {
        let x = 2;
        println!("{}", x);
    }
    println!("{}", x);
}

fn for_loop_var() {
    let items = vec![1, 2, 3];
    for item in items {
        println!("{}", item);
    }
}

fn while_loop_var() {
    let mut count = 0;
    while count < 10 {
        println!("{}", count);
        count += 1;
    }
}

fn multiple_refs() {
    let x = 1;
    let y = x + x;
    let z = x + y;
    println!("{}", z);
}
