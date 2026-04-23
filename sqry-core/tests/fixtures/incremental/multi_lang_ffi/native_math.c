/* Native arithmetic helpers exposed to Rust via extern "C". */

int native_add(int a, int b) {
    return a + b;
}

int native_multiply(int a, int b) {
    return a * b;
}
