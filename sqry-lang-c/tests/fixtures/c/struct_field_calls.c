// Struct field function calls

#include <stdio.h>

typedef struct {
    int (*callback)(int);
} Handler;

int double_value(int x) {
    return x * 2;
}

int triple_value(int x) {
    return x * 3;
}

void execute_handler(Handler* h, int value) {
    if (h->callback) {
        int result = h->callback(value);
        printf("Result: %d\n", result);
    }
}

void setup_handlers() {
    Handler h1 = { .callback = double_value };
    execute_handler(&h1, 10);

    Handler h2 = { .callback = triple_value };
    execute_handler(&h2, 10);
}
