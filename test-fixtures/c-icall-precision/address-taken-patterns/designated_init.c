/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic fixture for sqry C indirect-call precision (Phase A).
 * Exercises SPEC §3.1.1 row: function identifier as the value side of
 * a designated initializer pair (`.field = my_func`).
 *
 * Expected: pass5b's address-taken classifier MUST mark `my_read` as
 * address-taken. The `{ .read = my_read }` initializer is both the
 * address-take site AND a BindingEntry under
 * `(struct ops, read) -> my_read`.
 */

#include <stddef.h>

struct ops {
    int (*read)(int);
    int (*write)(int);
};

static int my_read(int x) {
    return x;
}

static const struct ops g_ops = {
    .read = my_read,
};
