/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic fixture for sqry C indirect-call precision (Phase A).
 * Exercises SPEC §3.1.1 row: function identifier on the RHS of an
 * assignment whose LHS is a `field_expression` of function-pointer type.
 *
 * Per SPEC §3.3.2 / DESIGN §7, runtime field assignment is explicitly
 * EXCLUDED from the binding plane — `my_read` is still address-taken,
 * but no BindingEntry is recorded.
 *
 * Expected: pass5b's address-taken classifier MUST mark `my_read` as
 * address-taken. The `fops->read = my_read` site is the address-take.
 */

struct ops {
    int (*read)(int);
};

static int my_read(int x) {
    return x;
}

void install(struct ops *fops) {
    fops->read = my_read;
}
