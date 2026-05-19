/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic fixture for sqry C indirect-call precision (Phase A).
 * Exercises SPEC §3.1.1 row: function identifier as the initializer of
 * a function-pointer declarator (`int (*fp)(int) = my_func;`).
 *
 * Expected: pass5b's address-taken classifier MUST mark `my_func` as
 * address-taken. The init-declarator `= my_func` site is the
 * address-take.
 */

static int my_func(int x) {
    return x;
}

void use(void) {
    int (*fp)(int) = my_func;
    (void)fp;
}
