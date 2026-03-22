// Test fixture for additional local variable scope patterns.
// Exercises: while-let, nested blocks, closures with match, slice patterns.

fn while_let_scope() {
    let items = vec![Some(1), None, Some(3)];
    let mut iter = items.into_iter();
    while let Some(val) = iter.next() {
        let doubled = val * 2;
        println!("{doubled}");
    }
}

fn nested_block_scope() {
    let outer = 1;
    {
        let inner = outer + 1;
        {
            let deep = inner + 1;
            println!("{deep}");
        }
    }
}

fn closure_with_match(data: Option<i32>) {
    let process = |opt: Option<i32>| match opt {
        Some(val) => val * 2,
        None => 0,
    };
    let result = process(data);
    println!("{result}");
}

fn slice_pattern_binding() {
    let numbers = [1, 2, 3, 4, 5];
    if let [first, .., last] = numbers {
        let sum = first + last;
        println!("{sum}");
    }
}

fn reference_pattern_binding() {
    let value = 42;
    let reference = &value;
    match reference {
        &x => println!("{x}"),
    }
}

fn or_pattern_binding() {
    let val: Result<i32, i32> = Ok(10);
    match val {
        Ok(n) | Err(n) => println!("{n}"),
    }
}
