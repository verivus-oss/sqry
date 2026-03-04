// Test fixture for C export extraction: structs, enums, typedefs
//
// Expected exports:
// - Rectangle (struct)
// - Circle (struct)
// - ShapeType (enum)
// - Color (typedef)
// - AreaFunc (typedef for function pointer)
// - global_shape_count (global variable)
// - init_shapes (function)
//
// NOT exported:
// - private_buffer (static variable)
// - internal_cleanup (static function)

#include <stdint.h>

// Struct definitions - exported
struct Rectangle {
    double width;
    double height;
};

struct Circle {
    double radius;
};

// Enum definition - exported
enum ShapeType {
    SHAPE_RECTANGLE,
    SHAPE_CIRCLE,
    SHAPE_POLYGON
};

// Typedef for primitive type - exported
typedef uint32_t Color;

// Typedef for function pointer - exported
typedef double (*AreaFunc)(void* shape);

// Global variable - exported
int global_shape_count = 0;

// Static variable - NOT exported
static char private_buffer[256];

// Static function - NOT exported
static void internal_cleanup(void) {
    global_shape_count = 0;
}

// Public function - exported
void init_shapes(void) {
    global_shape_count = 0;
    internal_cleanup();
}

// Public function - exported
double calculate_area(enum ShapeType type, void* shape) {
    if (type == SHAPE_RECTANGLE) {
        struct Rectangle* rect = (struct Rectangle*)shape;
        return rect->width * rect->height;
    } else if (type == SHAPE_CIRCLE) {
        struct Circle* circ = (struct Circle*)shape;
        return 3.14159 * circ->radius * circ->radius;
    }
    return 0.0;
}
