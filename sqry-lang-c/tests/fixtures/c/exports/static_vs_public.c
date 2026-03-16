// Test fixture for C export extraction: static vs public functions
//
// Expected exports:
// - create_user (public function)
// - delete_user (public function)
// - MAX_USERS (typedef)
// - UserRole (enum)
//
// NOT exported (static):
// - validate_id (static function)
// - internal_state (static variable)

#include <stdio.h>

#define MAX_ID 1000

// Static function - NOT exported
static int validate_id(int id) {
    return id > 0 && id < MAX_ID;
}

// Static variable - NOT exported
static int internal_state = 0;

// Public typedef - exported
typedef unsigned int MAX_USERS;

// Public enum - exported
enum UserRole {
    ADMIN,
    USER,
    GUEST
};

// Public function - exported
void create_user(int id, const char* name) {
    if (validate_id(id)) {
        printf("Creating user: %s\n", name);
        internal_state++;
    }
}

// Public function - exported
int delete_user(int id) {
    if (!validate_id(id)) {
        return -1;
    }
    internal_state--;
    return 0;
}
