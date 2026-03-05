// Function pointers

#include <stdio.h>

typedef int (*operation_t)(int, int);

int add(int a, int b) {
    return a + b;
}

int multiply(int a, int b) {
    return a * b;
}

int apply_operation(operation_t op, int x, int y) {
    return op(x, y);
}

void execute_operations() {
    operation_t op = add;
    int result1 = apply_operation(op, 5, 3);

    op = multiply;
    int result2 = apply_operation(op, 5, 3);

    // Direct function pointer call
    int result3 = (*op)(5, 3);
}
