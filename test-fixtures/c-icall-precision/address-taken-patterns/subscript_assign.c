/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic fixture for sqry C indirect-call precision (Phase A).
 * Exercises SPEC §3.1.1 row: function identifier on the RHS of an
 * assignment whose LHS is a `subscript_expression` of function-pointer
 * type.
 *
 * Expected: pass5b's address-taken classifier MUST mark `my_func` as
 * address-taken. The `table[0] = my_func` site is the address-take.
 * Subscript-assignment never produces a BindingEntry (DESIGN §7).
 */

typedef int (*handler_fn)(int);

static handler_fn table[4];

static int my_func(int x) {
    return x;
}

void install(void) {
    table[0] = my_func;
}
