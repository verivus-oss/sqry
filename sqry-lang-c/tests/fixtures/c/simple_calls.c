// Simple function calls

#include <stdio.h>

void helper() {
    printf("Helper function\n");
}

void main_function() {
    helper();
    printf("Main function\n");
}

int calculate(int a, int b) {
    return a + b;
}

void caller() {
    int result = calculate(5, 10);
    printf("Result: %d\n", result);
}
