// Test fixture for C import extraction: system headers
//
// Expected imports:
// - stdio.h (scope=system)
// - stdlib.h (scope=system)
// - string.h (scope=system)
// - sys/types.h (scope=system) - nested path
//
// This fixture tests extraction of system headers using <> delimiters.
// System headers are typically from the standard library or system directories.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>

// Duplicate include - should be deduplicated
#include <stdio.h>

void process_data() {
    printf("Processing...\n");
    char* buffer = (char*)malloc(256);
    strcpy(buffer, "test");
    free(buffer);
}

int main() {
    process_data();
    return 0;
}
