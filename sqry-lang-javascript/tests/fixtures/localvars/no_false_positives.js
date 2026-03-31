// No false positives tests

function typeNames(s) {
    // "length" in s.length should NOT be a local variable reference
    return s.length;
}

class MyClass {
    method() {
        // "method" should not be a local variable reference
        return 42;
    }
}
