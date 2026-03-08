// Static (file-local) functions

#include <stdio.h>

static void internal_helper() {
    printf("Internal helper\n");
}

void public_function() {
    internal_helper();
    printf("Public function\n");
}

static int calculate_internal(int x) {
    return x * 2;
}

int calculate_public(int x) {
    return calculate_internal(x) + 1;
}
