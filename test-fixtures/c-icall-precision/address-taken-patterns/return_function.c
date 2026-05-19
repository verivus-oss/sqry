/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic fixture for sqry C indirect-call precision (Phase A).
 * Exercises SPEC §3.1.1 row: function identifier in a value position
 * that is NOT the callee position (here: `return my_func`).
 *
 * Expected: pass5b's address-taken classifier MUST mark `my_func` as
 * address-taken. The `return my_func` site is the address-take.
 */

typedef int (*handler_fn)(int);

static int my_func(int x) {
    return x;
}

handler_fn get_handler(void) {
    return my_func;
}
