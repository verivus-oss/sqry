/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic fixture for sqry C indirect-call precision (Phase A).
 * Exercises SPEC §3.1.1 row: function identifier in a positional
 * initializer slot whose declared field type is a function pointer.
 *
 * Expected: pass5b's address-taken classifier MUST mark `my_read` as
 * address-taken. The positional `{ my_read, NULL }` initializer is the
 * address-take site AND a BindingEntry under (struct ops, read).
 */

#include <stddef.h>

struct ops {
    int (*read)(int);
    int (*write)(int);
};

static int my_read(int x) {
    return x;
}

static const struct ops g_ops = { my_read, NULL };
