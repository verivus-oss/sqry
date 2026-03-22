// Test fixture for C export extraction: forward declarations in header files
//
// Expected exports (declarations):
// - init_system (function declaration)
// - shutdown_system (function declaration)
// - get_status (function declaration)
// - Config (struct declaration)
// - Status (enum)
//
// Note: These are declarations, not definitions. The is_declaration
// metadata should be true for functions in .h files.

#ifndef FORWARD_DECLARATIONS_H
#define FORWARD_DECLARATIONS_H

// Forward struct declaration
struct Config;

// Function declarations (prototypes)
int init_system(struct Config* config);
void shutdown_system(void);
int get_status(void);

// Enum definition (even in header, this is a definition)
enum Status {
    STATUS_OK,
    STATUS_ERROR,
    STATUS_PENDING
};

// Typedef for struct
typedef struct Config Config;

#endif // FORWARD_DECLARATIONS_H
