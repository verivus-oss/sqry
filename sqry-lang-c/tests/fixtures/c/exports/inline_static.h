// Test fixture for C export extraction: static inline functions in headers
//
// Expected exports:
// - get_max (inline function - exported, marked as inline)
// - Point (struct)
// - distance (inline function - exported, marked as inline)
//
// NOT exported:
// - internal_helper (static inline - NOT exported due to static)
//
// Note: inline functions are typically exported unless marked static.
// static inline in headers means file-scope inline (not exported).

#ifndef INLINE_STATIC_H
#define INLINE_STATIC_H

#include <math.h>

// Static inline - NOT exported (file-scoped)
static inline int internal_helper(int x) {
    return x * 2;
}

// Inline function - exported (even in header)
inline int get_max(int a, int b) {
    return a > b ? a : b;
}

// Struct definition
struct Point {
    double x;
    double y;
};

// Inline function using struct - exported
inline double distance(struct Point* p1, struct Point* p2) {
    double dx = p2->x - p1->x;
    double dy = p2->y - p1->y;
    return sqrt(dx * dx + dy * dy);
}

// Another static inline - NOT exported
static inline void debug_log(const char* msg) {
    // Only for internal use in files that include this header
}

#endif // INLINE_STATIC_H
