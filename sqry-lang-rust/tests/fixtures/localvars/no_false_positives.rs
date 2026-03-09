// No false positives tests

fn type_names(s: String) -> usize {
    // "String" should NOT be a local variable reference
    // "usize" should NOT be a local variable reference
    s.len()
}

fn field_access() {
    struct Foo { bar: i32 }
    let f = Foo { bar: 42 };
    // "bar" in f.bar should NOT be a local variable reference
    let _result = f.bar;
}
