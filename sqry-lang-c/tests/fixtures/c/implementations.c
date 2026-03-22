// Function implementations (corresponding to declarations.h)

#include <stdio.h>
#include "declarations.h"

int calculate(int a, int b) {
    return a + b;
}

void print_result(int result) {
    printf("Result: %d\n", result);
}

void process_data(const char* data, int length) {
    for (int i = 0; i < length; i++) {
        printf("%c", data[i]);
    }
    printf("\n");
}

void main() {
    int result = calculate(10, 20);
    print_result(result);

    int sq = square(5);
    print_result(sq);
}
