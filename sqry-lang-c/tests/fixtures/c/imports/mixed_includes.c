// Test fixture for C import extraction: mixed system and local headers
//
// Expected imports:
// - stdio.h (scope=system)
// - stdlib.h (scope=system)
// - config.h (scope=local)
// - database.h (scope=local)
// - api/endpoints.h (scope=local) - nested path
//
// This fixture tests extraction of both system (<>) and local ("") headers
// in the same file, ensuring proper scope detection for each.

#include <stdio.h>
#include <stdlib.h>
#include "config.h"
#include "database.h"
#include "api/endpoints.h"

// Duplicate includes - should be deduplicated
#include <stdio.h>
#include "config.h"

typedef struct Config {
    char db_host[256];
    int db_port;
} Config;

Config* load_config() {
    Config* cfg = (Config*)malloc(sizeof(Config));
    snprintf(cfg->db_host, sizeof(cfg->db_host), "localhost");
    cfg->db_port = 5432;
    return cfg;
}

void connect_database(const Config* cfg) {
    printf("Connecting to %s:%d\n", cfg->db_host, cfg->db_port);
}

int main() {
    Config* cfg = load_config();
    connect_database(cfg);
    free(cfg);
    return 0;
}
