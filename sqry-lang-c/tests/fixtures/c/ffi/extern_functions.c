// FFI Test: Extern function declarations

// Standard library extern functions
extern int printf(const char *format, ...);
extern void *malloc(size_t size);
extern void free(void *ptr);

// Function that calls extern functions
void use_stdlib() {
    printf("Hello, World!\n");
    void *p = malloc(100);
    free(p);
}
