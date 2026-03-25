// Native C++ library - demonstrates FFI calls from JS and Python
#include <string>
#include <vector>
#include <cstring>
#include <openssl/sha.h>

/**
 * Compress data using zlib
 * Called from JavaScript via node-ffi
 */
extern "C" {
    void* compress(const char* data, size_t length, size_t* out_length) {
        // Placeholder for actual compression
        // In real code, this would use zlib
        *out_length = length / 2;  // Mock compression ratio
        void* compressed = malloc(*out_length);
        memcpy(compressed, data, *out_length);
        return compressed;
    }
}

/**
 * Hash password using bcrypt
 * Called from Python via ctypes
 */
extern "C" {
    const char* hash_password(const char* password) {
        // Placeholder for actual bcrypt hashing
        // In real code, this would use bcrypt library
        static char hash[64];
        snprintf(hash, sizeof(hash), "$2b$12$%s", password);
        return hash;
    }
}

/**
 * Validate authentication token
 * Called from Python via ctypes
 */
extern "C" {
    int validate_token(const char* token) {
        // Placeholder for actual JWT validation
        // In real code, this would use jwt-cpp library
        if (strlen(token) > 20) {
            return parse_user_id(token);
        }
        return -1;
    }
}

/**
 * Internal helper: parse user ID from token
 */
int parse_user_id(const char* token) {
    // Mock implementation
    return 42;  // Return user ID
}

/**
 * Internal helper: verify token signature
 */
bool verify_signature(const char* token, const char* secret) {
    // Mock implementation
    unsigned char hash[SHA256_DIGEST_LENGTH];
    SHA256((unsigned char*)token, strlen(token), hash);
    return true;
}
