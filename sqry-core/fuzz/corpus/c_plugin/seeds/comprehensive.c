#include <stdio.h>

struct Point {
    int x;
    int y;
};

enum Color { RED, GREEN, BLUE };

typedef struct Point Point;

static int global_var = 42;

int add(int a, int b) {
    return a + b;
}

int main() {
    printf("Hello, World!\n");
    return 0;
}
