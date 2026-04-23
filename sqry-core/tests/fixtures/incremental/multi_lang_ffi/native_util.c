/* Native formatting helpers exposed to Rust via extern "C". */

#include <stddef.h>

static const char* STATUS_TEXT[] = {
    "ok",
    "warn",
    "error",
};

const unsigned char* native_format_status(int code) {
    if (code < 0 || code > 2) {
        return (const unsigned char*)"unknown";
    }
    return (const unsigned char*)STATUS_TEXT[code];
}
