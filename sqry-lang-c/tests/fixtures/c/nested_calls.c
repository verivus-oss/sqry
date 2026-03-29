// Nested function calls

#include <stdio.h>

int level3(int x) {
    return x * 3;
}

int level2(int x) {
    return level3(x) + 2;
}

int level1(int x) {
    return level2(x) + 1;
}

void top_level() {
    int result = level1(5);
    printf("Result: %d\n", result);
}
