/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic fixture for sqry C indirect-call precision (Phase A).
 * Exercises SPEC §3.1.1 row: function identifier passed in a value
 * position that is NOT the callee position (here: as an argument).
 *
 * Expected: pass5b's address-taken classifier MUST mark `my_func` as
 * address-taken. The `register_callback(my_func)` site is the
 * address-take.
 */

typedef void (*callback_fn)(int);

static void my_func(int x) {
    (void)x;
}

void register_callback(callback_fn cb);

void init(void) {
    register_callback(my_func);
}
