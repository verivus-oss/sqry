/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic fixture for sqry C indirect-call precision (Phase A).
 * Exercises SPEC §3.1.1 row: unary `&` applied to a function identifier.
 *
 * Expected: pass5b's address-taken classifier MUST mark `my_func` as
 * address-taken. The `&my_func` site is the address-take.
 */

#include <stddef.h>

typedef void (*handler_fn)(int);

static void my_func(int x) {
    (void)x;
}

static handler_fn g_handler;

void init(void) {
    g_handler = &my_func;
}
