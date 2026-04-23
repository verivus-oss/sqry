/* Additional C-only helpers — no FFI surface. Used to exercise the
 * AddFile / RemoveFile operators on non-Rust files. */

int native_abs(int x) {
    if (x < 0) {
        return -x;
    }
    return x;
}

int native_clamp(int value, int low, int high) {
    if (value < low) {
        return low;
    }
    if (value > high) {
        return high;
    }
    return value;
}
