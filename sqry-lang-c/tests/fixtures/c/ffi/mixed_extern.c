// FFI Test: Mixed extern functions and variables

// Extern functions
extern int printf(const char *format, ...);
extern void *malloc(size_t size);
extern void free(void *ptr);

// Extern variables
extern int errno;
extern int optind;

// Regular (non-extern) function
void helper() {
    // This is a local function
}

// Function that calls both FFI and local functions
void mixed_calls() {
    printf("Starting\n");
    helper();
    void *p = malloc(50);
    if (errno) {
        printf("Error!\n");
    }
    free(p);
}

// Function that only calls local function
void local_only() {
    helper();
}
