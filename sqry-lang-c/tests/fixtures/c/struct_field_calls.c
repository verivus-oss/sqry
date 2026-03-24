// Struct field function calls and designated initializer function pointer dispatch

#include <stdio.h>

typedef struct {
    int (*callback)(int);
} Handler;

// Kernel-style vtable pattern
struct file_operations {
    int (*read)(void);
    int (*write)(int);
    int (*open)(void);
};

int double_value(int x) {
    return x * 2;
}

int triple_value(int x) {
    return x * 3;
}

int my_read(void) {
    return 0;
}

int my_write(int val) {
    return val;
}

int my_open(void) {
    return 1;
}

void execute_handler(Handler* h, int value) {
    if (h->callback) {
        int result = h->callback(value);
        printf("Result: %d\n", result);
    }
}

// Top-level designated initializer: creates References edges from
// my_fops -> my_read, my_fops -> my_write, my_fops -> my_open
const struct file_operations my_fops = {
    .read = my_read,
    .write = my_write,
    .open = my_open,
};

// Multi-variable declaration: each variable should only reference its own targets
int alt_read(void) { return -1; }
int alt_write(int v) { return -v; }

const struct file_operations fops_a = {
    .read = my_read,
    .write = my_write,
}, fops_b = {
    .read = alt_read,
    .write = alt_write,
};

void setup_handlers() {
    Handler h1 = { .callback = double_value };
    execute_handler(&h1, 10);

    Handler h2 = { .callback = triple_value };
    execute_handler(&h2, 10);
}
