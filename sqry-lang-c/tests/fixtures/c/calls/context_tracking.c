// Test fixture for C call extraction: context tracking
//
// Expected call edges:
// - outer → helper (caller: outer, callee: helper)
// - middle → helper (caller: middle, callee: helper)
// - middle → inner (caller: middle, callee: inner)
// - inner → helper (caller: inner, callee: helper)
// - main → outer (caller: main, callee: outer)
//
// This fixture tests that caller context is correctly tracked through nested calls.

void helper() {
    // Base helper function
}

void inner() {
    // Inner function calls helper
    helper();
}

void middle() {
    // Middle function calls both helper and inner
    helper();
    inner();
}

void outer() {
    // Outer function calls helper directly
    helper();
}

int main() {
    // Main calls outer (which then calls helper)
    outer();
    return 0;
}
