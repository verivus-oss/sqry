// FFI Test: Extern variable declarations

// Standard library extern variables
extern int errno;
extern char **environ;

// Function that uses extern variables
int check_error() {
    return errno;
}
