// Test fixture for C call extraction: inline static functions
//
// Expected call edges:
// - process → validate (caller: process, callee: validate)
// - validate → strlen (caller: validate, callee: strlen)
// - main → process (caller: main, callee: process)
//
// This fixture tests that calls within static functions are correctly tracked.

#include <string.h>

static int validate(const char* input) {
    // Call to stdlib function
    return input != NULL && strlen(input) > 0;
}

int process(const char* data) {
    // Call to static function
    if (validate(data)) {
        return 1;
    }
    return 0;
}

int main() {
    // Call to public function
    return process("test");
}
