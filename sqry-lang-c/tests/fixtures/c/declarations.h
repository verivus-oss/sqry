// Function declarations (header file)

#ifndef DECLARATIONS_H
#define DECLARATIONS_H

// Function declarations
int calculate(int a, int b);
void print_result(int result);
void process_data(const char* data, int length);

// Static inline function
static inline int square(int x) {
    return x * x;
}

#endif // DECLARATIONS_H
