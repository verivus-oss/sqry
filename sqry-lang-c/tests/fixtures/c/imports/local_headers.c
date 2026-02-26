// Test fixture for C import extraction: local headers
//
// Expected imports:
// - user.h (scope=local)
// - config.h (scope=local)
// - utils/helper.h (scope=local) - nested path
//
// This fixture tests extraction of local/project headers using "" delimiters.
// Local headers are typically from the same project or relative paths.

#include "user.h"
#include "config.h"
#include "utils/helper.h"

// Duplicate include - should be deduplicated
#include "user.h"

typedef struct User {
    int id;
    char name[100];
} User;

User* create_user(int id, const char* name) {
    User* user = (User*)malloc(sizeof(User));
    user->id = id;
    strncpy(user->name, name, sizeof(user->name) - 1);
    return user;
}

void destroy_user(User* user) {
    free(user);
}

int main() {
    User* u = create_user(1, "Alice");
    destroy_user(u);
    return 0;
}
